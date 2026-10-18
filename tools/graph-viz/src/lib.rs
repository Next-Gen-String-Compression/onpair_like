//! Draw a substring prefilter's alignment graph and minimum cut from a plan log.
//!
//! `LIKE '%pattern%'` over a compressed column is answered by first compiling a
//! *probe cover*: code ids such that every matching row must hold at least one
//! of them. The planner builds the DAG of every way the pattern can lie across
//! token boundaries and takes a cut of it. OnPair's `examples/plan_log.rs` and
//! the fsst fork's `search/tools/plan_log.cpp` log that DAG and cut per needle
//! and row group; this crate reads the log and draws it.
//!
//! ```no_run
//! use onpair_graph_viz::{Options, read_log, visualize};
//!
//! let log = read_log("out/plan-log".as_ref()).unwrap();
//! let figure = visualize(&log, &log.queries[0], &Options::default()).unwrap();
//! println!("{}", figure.svg.unwrap());
//! ```

#![deny(missing_docs)]

pub mod graph;
pub mod load;
pub mod render;
pub mod table;

pub use graph::{
    Cover, CoverShape, CoverTerm, EdgeKind, GraphEdge, LoggedGraph, PathGraph, Token,
    compile_cover, from_log,
};
pub use load::{Cut, Label, Log, LogEdge, Query, Symbols, read_log};
pub use render::{RenderSummary, predicate_text, render_svg};
pub use table::render_table_svg;

/// Default [`Options::max_states`]. One state is a column of the drawing about
/// 148 units wide, so this caps a figure near 20 000 units.
pub const DEFAULT_MAX_STATES: usize = 128;

/// How to draw the figure.
#[derive(Clone, Debug)]
pub struct Options {
    /// Refuse to render past this many byte-offset states. `None` always renders.
    pub max_states: Option<usize>,
    /// Headline for the figure. Defaults to dataset, row group and algorithm.
    pub title: Option<String>,
    /// Second line. Defaults to a preview of the pattern.
    pub subtitle: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_states: Some(DEFAULT_MAX_STATES),
            title: None,
            subtitle: None,
        }
    }
}

/// A rendered figure and everything behind it.
#[derive(Clone, Debug)]
pub struct Figure {
    /// The graph, the cut, and the edges taken as pairs.
    pub logged: LoggedGraph,
    /// The cover the cut's own edges compile to.
    pub cover: Cover,
    /// The header numbers.
    pub summary: RenderSummary,
    /// The SVG, or `None` when the graph exceeded [`Options::max_states`].
    pub svg: Option<String>,
    /// Every symbol of the row group, this cover's members marked.
    pub table_svg: String,
}

impl Figure {
    /// States the graph materialized, which [`Options::max_states`] limits.
    pub fn states(&self) -> usize {
        self.logged.graph.stats.unique_states
    }
}

/// Draw `query` from `log`.
///
/// # Errors
/// The log holds no symbol table or graph for the query, or an edge that does
/// not fit its label.
pub fn visualize(log: &Log, query: &Query, options: &Options) -> Result<Figure, String> {
    let symbols = log
        .symbols_of(query)
        .ok_or_else(|| format!("no symbol table for {}", query.uuid))?;
    let edges = log
        .graph
        .get(&query.uuid)
        .ok_or_else(|| format!("no graph for {}", query.uuid))?;
    let logged = from_log(query, edges, symbols)?;
    let graph = &logged.graph;

    let members = graph.membership_for_cut(&logged.cut);
    let cover = compile_cover(&members, &symbols.frequency);
    let title = options.title.clone().unwrap_or_else(|| {
        format!(
            "{} · row group {} · {}",
            query.dataset, query.row_group, query.algorithm
        )
    });
    let rows = query.rows as f64;
    let mut summary = RenderSummary::new(
        title.clone(),
        options
            .subtitle
            .clone()
            .unwrap_or_else(|| default_subtitle(&query.needle)),
        cover.terms.iter().map(|t| t.term_frequency).sum(),
        cover.shape,
    );
    summary.cover_terms = cover.terms.clone();
    summary.cover_pairs = logged.pairs.clone();
    summary.column_rows = Some(query.rows);
    summary.cut_value = logged
        .cut
        .iter()
        .filter_map(|&e| graph.edges[e].probe.as_ref())
        .map(|p| p.term_frequency)
        .sum();
    summary.contained_tokens = graph.contained.len();
    summary.contained_frequency = graph
        .contained
        .iter()
        .map(|&id| symbols.frequency[id as usize])
        .sum();
    summary.selectivity = Some(query.needle_selectivity);
    summary.exact_rows = Some((query.needle_selectivity * rows).round() as usize);
    summary.estimated_selectivity = Some(query.cut_selectivity_estimated);
    summary.candidate_selectivity = Some(query.cut_selectivity_measured);
    summary.cut_candidates = Some((query.cut_selectivity_measured * rows).round() as usize);

    let too_wide = options
        .max_states
        .is_some_and(|limit| graph.stats.unique_states > limit);
    let svg = (!too_wide).then(|| render_svg(graph, &logged.cut, &logged.paired, &summary));
    let table_svg = render_table_svg(symbols, graph, &cover.members, &title);

    Ok(Figure {
        logged,
        cover,
        summary,
        svg,
        table_svg,
    })
}

fn default_subtitle(pattern: &[u8]) -> String {
    match std::str::from_utf8(pattern) {
        Ok(text) => format!("contains \"{text}\""),
        Err(_) => format!("contains {} raw bytes", pattern.len()),
    }
}
