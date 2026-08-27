//! Draw OnPair's substring-prefilter reasoning for any column and any pattern.
//!
//! `LIKE '%pattern%'` over an OnPair column is answered by first compiling a
//! *probe cover*: a set of dictionary token ids such that every matching row must
//! hold at least one of them. Rows without a probe cannot match and are never
//! decoded. Which cover you get is a minimization: build the DAG of every way the
//! pattern can lie across token boundaries, then take its cheapest weighted
//! vertex cut. This crate renders that DAG and that cut.
//!
//! # Input
//! Whatever you already have — a [`ColumnView`] and the [`TokenFrequencyIndex`]
//! built for it:
//!
//! ```no_run
//! use onpair::search::index::build_token_frequency_index;
//! use onpair::{Column, DEFAULT_CONFIG, DictionaryView};
//! use onpair_graph_viz::{Options, visualize};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let rows: Vec<&str> = vec!["/search?q=onpair", "/index.html"];
//! let bytes: Vec<u8> = rows.concat().into_bytes();
//! let mut offsets = vec![0u32];
//! for row in &rows {
//!     offsets.push(offsets.last().unwrap() + row.len() as u32);
//! }
//!
//! let column = Column::compress(&bytes, &offsets, DEFAULT_CONFIG)?;
//! let view = column.view();
//! let frequencies = build_token_frequency_index(view.codes, view.dict.num_tokens())?;
//!
//! let figure = visualize(view, &frequencies, b"search", &Options::default())?;
//! println!("{}", figure.svg.unwrap());
//! # Ok(())
//! # }
//! ```
//!
//! No dataset, benchmark or workload is baked in; a column is a column.
//!
//! # Why this re-derives the graph
//! OnPair's planner keeps only what the cut needs — ids and weights — because it
//! runs per query. A figure needs the opposite: token strings, alignment
//! provenance, per-probe statistics, labels. Rather than ask the library to carry
//! that, this crate rebuilds the same DAG from OnPair's *public* API
//! ([`prefix_range`](onpair::search::prefix_range) over the dictionary, plus the
//! frequency index) and is free to be as verbose as a picture wants. The cost is
//! duplicated logic, kept honest by checking each rendered cover against ground
//! truth — see [`Measurement`].
//!
//! # Keeping up with the pin
//! `tools/onpair-artifact/Cargo.toml` owns the one `onpair` revision used by
//! both the benchmark candidate and this renderer. The figures are only as
//! truthful as this crate's copy of that revision's planning rules. Everything the
//! library exposes is called, not reimplemented — the dictionary, `prefix_range`,
//! the frequency index, [`BytesVerifier`], `prefilter_candidates` — but the DAG,
//! the cut and the cover shape are re-derived, so **moving the pin means reviewing
//! them**. The two places that copy a private rule say so at their definitions:
//! [`live_cover`] (run merging and comparison pricing) and [`mincut`].
//! [`Measurement`] catches divergence that changes which rows are admitted; it
//! cannot catch divergence that only changes zero-frequency probes.

#![deny(missing_docs)]

pub mod graph;
pub mod mincut;
pub mod render;

use onpair::search::index::{
    TokenFrequencyIndex, TokenFrequencyIndexError, TokenFrequencyIndexStorage,
    build_token_frequency_index,
};
use onpair::search::{BytesVerifier, PrefilterError, analyze_prefilter, prefilter_candidates};
use onpair::{ColumnView, DictionaryView, Offset};
use serde::Serialize;

pub use graph::{
    CoverShape, LiveCover, PathGraph, ProbeWeight, Weights, build_path_graph, live_cover,
};
pub use mincut::{CutResult, minimum_vertex_cut};
pub use render::{RenderSummary, render_svg};

/// Default [`Options::max_states`]. One state is a column of the drawing about
/// 148 units wide, so this caps a figure near 20 000 units — already far past
/// what reads well on a page, and short of the point where an SVG gets silly.
pub const DEFAULT_MAX_STATES: usize = 128;

/// Why a figure could not be produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The empty pattern matches every row at offset 0; there is nothing to cut.
    EmptyPattern,
    /// The frequency index describes a different dictionary.
    IndexMismatch {
        /// Tokens the index covers.
        index_tokens: usize,
        /// Tokens the dictionary holds.
        dict_tokens: usize,
    },
    /// The index could not be built for this column.
    Index(TokenFrequencyIndexError),
    /// Candidate/exact-row measurement needs a code stream and row offsets.
    MeasurementNeedsColumn,
    /// Document-frequency metrics need a code stream and row offsets.
    RowMetricNeedsColumn,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPattern => f.write_str("the empty pattern has no alignment graph"),
            Self::IndexMismatch {
                index_tokens,
                dict_tokens,
            } => write!(
                f,
                "frequency index covers {index_tokens} tokens, dictionary holds {dict_tokens}"
            ),
            Self::Index(error) => write!(f, "{error}"),
            Self::MeasurementNeedsColumn => {
                f.write_str("measurement needs a full column; use --no-measure with a sidecar")
            }
            Self::RowMetricNeedsColumn => f.write_str("row-frequency metrics need a full column"),
        }
    }
}

impl std::error::Error for Error {}

impl From<TokenFrequencyIndexError> for Error {
    fn from(error: TokenFrequencyIndexError) -> Self {
        Self::Index(error)
    }
}

/// How to build and draw the figure.
#[derive(Clone, Debug)]
pub struct Options {
    /// Which frequency the cut minimizes.
    pub metric: ProbeWeight,
    /// Refuse to render past this many byte-offset states, emitting the graph
    /// without an SVG. `None` always renders.
    pub max_states: Option<usize>,
    /// Headline for the figure — the column's name, usually.
    pub title: String,
    /// Second line. Defaults to a preview of the pattern.
    pub subtitle: Option<String>,
    /// Measure the cover against the column: candidate rows, true matches, and
    /// what OnPair's own prefilter admits. Costs one code-stream scan plus a
    /// decode of the candidates.
    pub measure: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            metric: ProbeWeight::TermFrequency,
            max_states: Some(DEFAULT_MAX_STATES),
            title: String::new(),
            subtitle: None,
            measure: true,
        }
    }
}

/// What the cover actually does on the column it was compiled for.
///
/// This is the crate's own check on itself. The DAG here is a second
/// implementation of OnPair's planning logic, so a rendered cover that is *not*
/// sound means the picture is lying, and `sound` says so rather than leaving it
/// to be noticed later.
#[derive(Clone, Debug, Serialize)]
pub struct Measurement {
    /// Rows holding at least one probe token — what a scan would hand the verifier.
    pub candidates: usize,
    /// Rows that really contain the pattern, by decode-and-search.
    pub exact_rows: usize,
    /// Whether every true match is among the candidates. Must be true.
    pub sound: bool,
    /// Candidates OnPair's own prefilter admits for the same pattern.
    ///
    /// Equal counts mean both implementations picked covers of the same
    /// selectivity. They may legitimately differ when several cuts tie on weight,
    /// so a difference is worth reading, not a failure.
    pub onpair_candidates: Option<usize>,
    /// Why OnPair's prefilter declined, if it did.
    pub onpair_refusal: Option<String>,
}

/// A rendered figure and everything behind it.
#[derive(Clone, Debug, Serialize)]
pub struct Figure {
    /// The alignment DAG.
    pub graph: PathGraph,
    /// Its minimum cut under [`Options::metric`].
    pub cut: CutResult,
    /// The cover's point/range shape and SIMD comparison cost.
    pub cover: CoverShape,
    /// Cut nodes removed by cover normalization. Empty for the pinned planner,
    /// which preserves membership even when advisory frequency is zero.
    pub dead_probes: Vec<usize>,
    /// The header numbers.
    pub summary: RenderSummary,
    /// Measurements, when [`Options::measure`] was set.
    pub measurement: Option<Measurement>,
    /// The SVG, or `None` when the graph exceeded [`Options::max_states`].
    #[serde(skip)]
    pub svg: Option<String>,
}

impl Figure {
    /// States the graph materialized — what [`Options::max_states`] limits.
    pub fn states(&self) -> usize {
        self.graph.stats.unique_states
    }
}

/// Build the alignment DAG for `pattern`, cut it, and draw it.
///
/// `frequencies` must have been built for `view`
/// ([`build_token_frequency_index`](onpair::search::build_token_frequency_index)).
///
/// # Errors
/// [`Error::EmptyPattern`] for an empty pattern, [`Error::IndexMismatch`] if the
/// index does not describe this column's dictionary.
pub fn visualize<O: Offset, S: TokenFrequencyIndexStorage>(
    view: ColumnView<'_, O>,
    frequencies: &TokenFrequencyIndex<S>,
    pattern: &[u8],
    options: &Options,
) -> Result<Figure, Error> {
    let weights = if options.metric.needs_rows() {
        Weights::from_column(view, frequencies)?
    } else {
        Weights::from_index(frequencies)
    };
    let total_rows = view.num_rows();
    visualize_with_weights(
        view.dict,
        frequencies,
        pattern,
        options,
        weights,
        |members| {
            options
                .measure
                .then(|| measure(view, frequencies, pattern, members))
        },
        Some(total_rows),
    )
}

/// Build a graph from the exact dictionary and frequency index alone.
///
/// This is the sidecar entry point. Term-frequency objectives are exact;
/// measurement and row-frequency objectives require the omitted code stream.
pub fn visualize_index<D: DictionaryView, S: TokenFrequencyIndexStorage>(
    dictionary: D,
    frequencies: &TokenFrequencyIndex<S>,
    pattern: &[u8],
    options: &Options,
) -> Result<Figure, Error> {
    if options.measure {
        return Err(Error::MeasurementNeedsColumn);
    }
    if options.metric.needs_rows() {
        return Err(Error::RowMetricNeedsColumn);
    }
    visualize_with_weights(
        dictionary,
        frequencies,
        pattern,
        options,
        Weights::from_index(frequencies),
        |_| None,
        None,
    )
}

fn visualize_with_weights<
    D: DictionaryView,
    S: TokenFrequencyIndexStorage,
    F: FnOnce(&[bool]) -> Option<Measurement>,
>(
    dictionary: D,
    frequencies: &TokenFrequencyIndex<S>,
    pattern: &[u8],
    options: &Options,
    weights: Weights,
    measure_cover: F,
    total_rows: Option<usize>,
) -> Result<Figure, Error> {
    let graph = build_path_graph(dictionary, pattern, &weights)?;
    let cut = minimum_vertex_cut(&graph, options.metric);

    // The cut's ids plus mandatory whole-pattern ids, normalized into the same
    // point/range cover OnPair issues to its SIMD scan.
    let members = graph.membership_for_cut(&cut.selected_nodes);
    let live = live_cover(&members, frequencies);
    let dead_probes = dead_probes(&graph, &cut.selected_nodes, &live);
    let cover_frequency = weights.membership_term_frequency(&live.members);

    // The mandatory ids on their own, so the figure can say where the difference
    // between the cut's weight and the cover's frequency comes from.
    let mut contained_members = vec![false; graph.dictionary_size];
    for &id in &graph.contained {
        contained_members[id as usize] = true;
    }

    let measurement = measure_cover(&live.members);

    let mut summary = RenderSummary::new(
        options.title.clone(),
        options
            .subtitle
            .clone()
            .unwrap_or_else(|| default_subtitle(pattern)),
        options.metric,
        cover_frequency,
        live.shape,
    );
    summary.cut_value = cut.value;
    summary.contained_tokens = graph.contained.len();
    summary.contained_frequency = weights.membership_term_frequency(&contained_members);
    summary.dead_probes = dead_probes.len();
    if let Some(measurement) = &measurement {
        summary.cut_candidates = Some(measurement.candidates);
        summary.exact_rows = Some(measurement.exact_rows);
        summary.selectivity = total_rows
            .filter(|&rows| rows > 0)
            .map(|rows| measurement.exact_rows as f64 / rows as f64);
    }

    let too_wide = options
        .max_states
        .is_some_and(|limit| graph.stats.unique_states > limit);
    let svg = (!too_wide).then(|| render_svg(&graph, &cut.selected_nodes, &dead_probes, &summary));

    Ok(Figure {
        graph,
        cut,
        cover: live.shape,
        dead_probes,
        summary,
        measurement,
        svg,
    })
}

/// The cut nodes with no surviving id in `live` — selected by the cut, dropped by
/// the planner. Ascending, as [`CutResult::selected_nodes`] is.
fn dead_probes(graph: &PathGraph, selected_nodes: &[usize], live: &LiveCover) -> Vec<usize> {
    selected_nodes
        .iter()
        .copied()
        .filter(|&node| {
            let mut alive = false;
            graph.nodes[node]
                .probe
                .as_ref()
                .expect("cut selected a non-probe node")
                .set
                .for_each(|id| alive |= live.members[id as usize]);
            !alive
        })
        .collect()
}

/// Rows holding at least one token from `members`.
pub fn candidate_rows<O: Offset>(view: ColumnView<'_, O>, members: &[bool]) -> Vec<usize> {
    (0..view.num_rows())
        .filter(|&row| {
            view.row_codes(row)
                .iter()
                .any(|&code| members[code as usize])
        })
        .collect()
}

fn measure<O: Offset, S: TokenFrequencyIndexStorage>(
    view: ColumnView<'_, O>,
    frequencies: &TokenFrequencyIndex<S>,
    pattern: &[u8],
    members: &[bool],
) -> Measurement {
    let candidates = candidate_rows(view, members);

    let mut exact: Vec<usize> = (0..view.num_rows()).collect();
    BytesVerifier::new(pattern).retain(view, &mut exact);

    // `candidates` is ascending, so a merge walk decides containment in one pass.
    let mut cursor = candidates.iter().copied().peekable();
    let mut sound = true;
    for row in &exact {
        while cursor.peek().is_some_and(|&candidate| candidate < *row) {
            cursor.next();
        }
        if cursor.next_if_eq(row).is_none() {
            sound = false;
            break;
        }
    }

    let mut theirs = Vec::new();
    let analysis = analyze_prefilter(pattern, view.dict, frequencies);
    let refusal = prefilter_candidates(view.codes, view.row_offsets, &analysis, &mut theirs).err();

    Measurement {
        candidates: candidates.len(),
        exact_rows: exact.len(),
        sound,
        onpair_candidates: refusal.is_none().then_some(theirs.len()),
        onpair_refusal: refusal.map(refusal_text),
    }
}

fn refusal_text(error: PrefilterError) -> String {
    error.to_string()
}

fn default_subtitle(pattern: &[u8]) -> String {
    match std::str::from_utf8(pattern) {
        Ok(text) => format!("contains \"{text}\""),
        Err(_) => format!("contains {} raw bytes", pattern.len()),
    }
}

/// Build the frequency index for a column, as the tool's callers usually need it.
///
/// # Errors
/// Whatever [`build_token_frequency_index`](onpair::search::index::build_token_frequency_index)
/// refused with.
pub fn index_for<O: Offset>(view: ColumnView<'_, O>) -> Result<TokenFrequencyIndex, Error> {
    Ok(build_token_frequency_index(
        view.codes,
        view.dict.num_tokens(),
    )?)
}
