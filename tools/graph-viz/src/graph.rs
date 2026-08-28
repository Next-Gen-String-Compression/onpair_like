//! The alignment DAG, rebuilt for drawing.
//!
//! Same structure OnPair's planner builds — one lane per feasible first-token
//! alignment, lanes merging wherever greedy tokenization reaches the same needle
//! byte offset — but carrying the things a figure needs and a planner must not
//! pay for: token strings, human labels, per-probe term *and* row frequencies,
//! and convergence counters.
//!
//! Nodes come in two flavours. **Probe** nodes carry a [`Probe`] and are the only
//! ones a cut may select; their capacity is the probe's weight. **Structural**
//! nodes (source, sink, alignment junctions, byte-offset states) carry no probe
//! and are uncuttable. A cut of this DAG is exactly a sound probe cover for the
//! boundary-crossing case, which is what makes the picture worth looking at.


use onpair::search::index::TokenFrequencyIndex;
use onpair::search::prefix_range;
use onpair::{ColumnView, DictionaryView, MAX_TOKEN_SIZE, Offset, Token, TokenRange};
use serde::Serialize;

use crate::Error;

/// Bytes of a token the figure spells out. [`MAX_TOKEN_SIZE`] is the whole of
/// one, so no token is ever shown cut — cards size themselves to their text
/// instead of the text shrinking to the card. An escaped byte still renders as
/// four characters, so the *character* count can exceed this.
const TOKEN_PREVIEW: usize = MAX_TOKEN_SIZE;

/// Ids of one probe the figure names, heaviest first. The rest become an "and N
/// more" tail: a first-token set holds at most [`SET_CAP`] ids, so the tail is
/// short and the reader still sees what dominates the probe's cost.
const TOKEN_LIST_CAP: usize = 10;

/// Largest first-token set drawn as one explicit probe. Beyond this an alignment
/// is entered unprobed and the cut has to catch it further down the lane, which
/// is exactly what OnPair does with its own `SET_CAP`.
pub const SET_CAP: usize = 16;

/// Which frequency a cut minimizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeWeight {
    /// Token occurrences in the code stream — what OnPair's planner minimizes.
    TermFrequency,
    /// As [`TermFrequency`](Self::TermFrequency), but tokens that already contain
    /// the whole needle count as free: they are mandatory members, so charging
    /// for them twice distorts the choice.
    ResidualTermFrequency,
    /// Rows holding the token, summed over the probe's ids. Closer to candidate
    /// count, and not something OnPair's index can answer.
    RowFrequency,
    /// [`RowFrequency`](Self::RowFrequency) with mandatory tokens made free.
    ResidualRowFrequency,
}

impl ProbeWeight {
    /// Every metric, in declaration order.
    pub const ALL: [Self; 4] = [
        Self::TermFrequency,
        Self::ResidualTermFrequency,
        Self::RowFrequency,
        Self::ResidualRowFrequency,
    ];

    /// Short name, as accepted on the command line.
    pub const fn name(self) -> &'static str {
        match self {
            Self::TermFrequency => "tf",
            Self::ResidualTermFrequency => "tf_residual",
            Self::RowFrequency => "df",
            Self::ResidualRowFrequency => "df_residual",
        }
    }

    /// Parse a [`name`](Self::name).
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|metric| metric.name() == text)
    }

    /// Whether this metric needs row frequencies, which only a column can supply.
    pub const fn needs_rows(self) -> bool {
        matches!(self, Self::RowFrequency | Self::ResidualRowFrequency)
    }
}

/// The token ids one probe tests for.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProbeSet {
    /// One id.
    Point {
        /// The id.
        id: Token,
    },
    /// An inclusive id range, as produced by a dictionary prefix search.
    Range {
        /// First id.
        lo: Token,
        /// Last id.
        hi: Token,
    },
    /// An enumerated set — the tokens ending with some needle prefix, which are
    /// scattered through the dictionary rather than contiguous.
    Explicit {
        /// The ids, ascending.
        ids: Vec<Token>,
    },
}

impl ProbeSet {
    /// A single-id set.
    pub fn point(id: Token) -> Self {
        Self::Point { id }
    }

    /// A set covering a dictionary prefix range.
    pub fn range(range: TokenRange) -> Self {
        Self::Range {
            lo: range.begin,
            hi: range.last,
        }
    }

    /// Visit every id in the set.
    pub fn for_each(&self, mut f: impl FnMut(Token)) {
        match self {
            Self::Point { id } => f(*id),
            Self::Range { lo, hi } => {
                for id in *lo..=*hi {
                    f(id);
                }
            }
            Self::Explicit { ids } => ids.iter().copied().for_each(f),
        }
    }

    /// How many ids the set holds.
    pub fn len(&self) -> usize {
        match self {
            Self::Point { .. } => 1,
            Self::Range { lo, hi } => *hi as usize - *lo as usize + 1,
            Self::Explicit { ids } => ids.len(),
        }
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total weight of the set under a cumulative per-token weight array.
    fn prefix_weight(&self, cumulative: &[u64]) -> u64 {
        match self {
            Self::Point { id } => cumulative[*id as usize + 1] - cumulative[*id as usize],
            Self::Range { lo, hi } => cumulative[*hi as usize + 1] - cumulative[*lo as usize],
            Self::Explicit { ids } => ids
                .iter()
                .map(|&id| cumulative[id as usize + 1] - cumulative[id as usize])
                .sum(),
        }
    }
}

/// One testable set of token ids, weighted four ways and labelled for drawing.
#[derive(Clone, Debug, Serialize)]
pub struct Probe {
    /// The ids this probe tests for.
    pub set: ProbeSet,
    /// Token occurrences in the code stream.
    pub term_frequency: u64,
    /// Rows containing at least one of the ids, summed per id.
    pub row_frequency: u64,
    /// [`term_frequency`](Self::term_frequency) with mandatory tokens zeroed.
    pub residual_term_frequency: u64,
    /// [`row_frequency`](Self::row_frequency) with mandatory tokens zeroed.
    pub residual_row_frequency: u64,
    /// Short label, drawn on the card.
    pub label: String,
    /// Longer explanation, drawn under the label and used as the SVG tooltip.
    pub detail: String,
    /// Up to [`TOKEN_LIST_CAP`] of the set's ids, term frequency descending, for
    /// the figure to name — an explicit set is a heap of unrelated tokens, and
    /// its id count alone says nothing about what it will match.
    ///
    /// Empty for the other set shapes: a point is already named by its token, and
    /// a range is named by its two ends and may span the whole dictionary.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub heaviest: Vec<Token>,
}

impl Probe {
    /// This probe's weight under `metric`.
    pub const fn weight(&self, metric: ProbeWeight) -> u64 {
        match metric {
            ProbeWeight::TermFrequency => self.term_frequency,
            ProbeWeight::ResidualTermFrequency => self.residual_term_frequency,
            ProbeWeight::RowFrequency => self.row_frequency,
            ProbeWeight::ResidualRowFrequency => self.residual_row_frequency,
        }
    }
}

/// What a parse step is: the three shapes an edge of the DAG can have.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EdgeKind {
    /// Entering alignment `k`: the occurrence's first token ends with
    /// `needle[..k]`, so those bytes are covered before the parse starts.
    Entry {
        /// The alignment's `k`.
        alignment: usize,
        /// Tokens that qualify as that first token — reported even when there are
        /// more than [`SET_CAP`] of them and the step therefore carries no probe.
        qualifying: usize,
    },
    /// The one interior token greedy parsing takes, covering `needle[from..to]`.
    Step {
        /// That token.
        token: Token,
    },
    /// Tokens the remaining needle is a prefix of: reaching one is a match.
    Terminal,
    /// Tokens whose bytes hold the *whole* needle. Such an occurrence crosses no
    /// boundary, so this step runs start to accept in one — and being the only
    /// step on that path, every cut takes it.
    Contained {
        /// How many tokens hold it.
        tokens: usize,
    },
    /// No token is a prefix of what remains, so the parse can only continue
    /// through an escape. The dictionary's escape code is what a scan looks for,
    /// and the rest of the lane is not traced.
    Escape,
}

/// One parse step, and the probe that catches every layout crossing it.
///
/// A node is a needle offset and an edge is a step, so probing is a property of
/// the step — *look for this token set and the step cannot pass unnoticed* — and a
/// cut selects edges.
#[derive(Clone, Debug, Serialize)]
pub struct GraphEdge {
    /// Index of this edge in [`PathGraph::edges`].
    pub id: usize,
    /// Needle offset the step leaves; `0` is the start.
    pub from: usize,
    /// Needle offset it reaches; `needle.len()` is the accept position.
    pub to: usize,
    /// Which of the three shapes it is.
    pub kind: EdgeKind,
    /// The set a scan looks for. `None` on a step no cut may select — a
    /// first-token set past [`SET_CAP`], never materialized, so the step is
    /// entered free and the cut pays further along the chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<Probe>,
}

/// Shape counters worth reporting next to a figure.
#[derive(Clone, Debug, Default, Serialize)]
pub struct GraphStats {
    /// Alignments a layout can enter through: `k = 0`, plus every `k` some token
    /// ends with `needle[..k]`.
    pub feasible_alignments: usize,
    /// Offsets a parse step leaves or reaches.
    pub unique_states: usize,
    /// States more than one step reaches — where lanes converge, and the reason a
    /// global cut beats choosing per alignment.
    pub converged_states: usize,
    /// Steps into the accept position: the ways an occurrence can end.
    pub terminal_paths: usize,
    /// Edges a cut may select.
    pub selectable_probe_edges: usize,
    /// States with no step out and no way to accept, where a lane simply ends —
    /// what an incomplete alphabet produces.
    pub dead_end_states: usize,
}

/// The alignment DAG for one needle over one dictionary.
///
/// A node *is* a needle offset: node `o` is the parse position at `needle[o..]`,
/// node `0` is the start and node `needle.len()` the accept position. Two
/// alignments reaching the same offset therefore land on the same node — state
/// merging is what the numbering means, not something the builder arranges.
#[derive(Clone, Debug, Serialize)]
pub struct PathGraph {
    /// The needle this graph was built for.
    pub needle: Vec<u8>,
    /// Every parse step.
    pub edges: Vec<GraphEdge>,
    /// Offsets any step leaves or reaches, ascending, including the accept
    /// position. Offsets no parse reaches are not here — they are isolated nodes.
    pub states: Vec<usize>,
    /// Token ids containing the whole needle, for the figure to name. They are in
    /// the DAG now — as one start-to-accept step — so this is a label, not a
    /// separate rule: the cut selects that step like any other.
    pub contained: Vec<Token>,
    /// The dictionary's escape code, when it has one. Not a token: it names a
    /// byte the encoder could not tokenize, and the figure spells it `<ESCAPE>`.
    pub escape_token: Option<Token>,
    /// Ids a code can take, which is the frequency index's width rather than the
    /// dictionary's — an escaped byte rides in the stream as a code of its own.
    pub code_space: usize,
    /// Shape counters.
    pub stats: GraphStats,
    /// Dictionary token count.
    pub dictionary_size: usize,
    /// Total token occurrences in the code stream the weights came from.
    pub total_codes: u64,
    /// Rows the weights counted, or zero when they came from an index alone —
    /// which is what says whether a row *frequency* was counted at all.
    pub total_rows: u64,
    /// Rows in the column, whether or not row frequencies were counted. A token's
    /// occurrences over this is the row share the planner is implicitly pricing —
    /// an upper bound on it, since a row can hold the token twice — and it is what
    /// the figure shows as `RF`. Zero when no column was in hand.
    pub column_rows: u64,
    /// Every dictionary token, previewed and escaped, indexed by id — so the
    /// drawing can name a probe by what it matches rather than by a number.
    ///
    /// Not serialized: ids stay the JSON's cross-reference, and a whole
    /// dictionary would dwarf the graph it is attached to.
    #[serde(skip)]
    pub token_text: Vec<String>,
}

impl PathGraph {
    /// The accept position: every needle byte consumed.
    pub fn end(&self) -> usize {
        self.needle.len()
    }

    /// What cutting `edge` costs under `metric`, or `None` if no cut may select
    /// it — which the solver reads as a capacity no finite cut can reach.
    pub fn edge_capacity(&self, edge: &GraphEdge, metric: ProbeWeight) -> Option<u64> {
        edge.probe.as_ref().map(|probe| probe.weight(metric))
    }

    /// The token ids a cut selects.
    ///
    /// Cutting an edge means probing for its set, so the cover is the union of the
    /// selected sets — the whole-needle tokens included, since their step is one a
    /// cut has to take.
    pub fn membership_for_cut(&self, selected_edges: &[usize]) -> Vec<bool> {
        let mut members = vec![false; self.code_space];
        for &edge in selected_edges {
            self.edges[edge]
                .probe
                .as_ref()
                .expect("cut selected an unprobed step")
                .set
                .for_each(|id| members[id as usize] = true);
        }
        members
    }
}

/// Per-token weights, as prefix sums so a range costs two lookups.
///
/// Term frequencies come straight out of OnPair's own index, so a figure's
/// numbers are the ones its planner actually optimized. Row frequencies are not
/// in that index — a column is needed to count them.
#[derive(Clone, Debug)]
pub struct Weights {
    cum_tf: Vec<u64>,
    cum_df: Vec<u64>,
    total_rows: u64,
}

impl Weights {
    /// Term frequencies from `frequencies`; row frequencies counted from `view`.
    ///
    /// # Errors
    /// [`Error::IndexMismatch`] if the index does not describe this column.
    pub fn from_column<O: Offset>(
        view: ColumnView<'_, O>,
        frequencies: &TokenFrequencyIndex,
    ) -> Result<Self, Error> {
        let num_tokens = view.dict.num_tokens();
        if frequencies.num_tokens() != num_tokens {
            return Err(Error::IndexMismatch {
                index_tokens: frequencies.num_tokens(),
                dict_tokens: num_tokens,
            });
        }

        let mut df = vec![0u64; num_tokens];
        let mut last_row = vec![usize::MAX; num_tokens];
        for row in 0..view.num_rows() {
            for &code in view.row_codes(row) {
                let id = code as usize;
                if last_row[id] != row {
                    last_row[id] = row;
                    df[id] += 1;
                }
            }
        }

        Ok(Self {
            cum_tf: cumulative_from_index(frequencies),
            cum_df: cumulative(&df),
            total_rows: view.num_rows() as u64,
        })
    }

    /// Term frequencies only. Row-frequency metrics silently reuse them, which
    /// is why [`from_column`](Self::from_column) is the one to prefer.
    pub fn from_index(frequencies: &TokenFrequencyIndex) -> Self {
        let cum_tf = cumulative_from_index(frequencies);
        Self {
            cum_df: cum_tf.clone(),
            cum_tf,
            total_rows: 0,
        }
    }

    /// Dictionary size these weights were built for.
    pub fn num_tokens(&self) -> usize {
        self.cum_tf.len() - 1
    }

    /// Total token occurrences.
    pub fn total_codes(&self) -> u64 {
        self.cum_tf[self.num_tokens()]
    }

    /// Term-frequency prefix sums, one entry longer than the dictionary.
    pub fn cum_tf(&self) -> &[u64] {
        &self.cum_tf
    }

    /// Row-frequency prefix sums, or the term ones when no rows were counted.
    pub fn cum_df(&self) -> &[u64] {
        &self.cum_df
    }

    /// One token's occurrences in the code stream.
    pub fn token_term_frequency(&self, id: Token) -> u64 {
        let index = id as usize;
        self.cum_tf[index + 1] - self.cum_tf[index]
    }

    /// Total weight of a membership table under the term-frequency metric.
    pub fn membership_term_frequency(&self, members: &[bool]) -> u64 {
        membership_frequency(members, &self.cum_tf)
    }
}

fn cumulative_from_index(frequencies: &TokenFrequencyIndex) -> Vec<u64> {
    let per_token: Vec<u64> = (0..frequencies.num_tokens())
        .map(|id| u64::from(frequencies.frequency(id as Token)))
        .collect();
    cumulative(&per_token)
}

/// Exclusive prefix sums, one longer than the input.
pub fn cumulative(values: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(values.len() + 1);
    out.push(0);
    for &value in values {
        let next = out.last().copied().unwrap() + value;
        out.push(next);
    }
    out
}

/// Total weight of the selected ids.
pub fn membership_frequency(members: &[bool], cumulative: &[u64]) -> u64 {
    members
        .iter()
        .enumerate()
        .filter(|(_, selected)| **selected)
        .map(|(id, _)| cumulative[id + 1] - cumulative[id])
        .sum()
}

/// How a membership table looks once expressed as points and ranges.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct CoverShape {
    /// Token ids probed for.
    pub member_ids: usize,
    /// Runs of length one.
    pub points: usize,
    /// Runs of length two or more.
    pub ranges: usize,
    /// SIMD comparisons the cover costs: one per point, one per range.
    pub cmp_cost: usize,
}

/// One probe the scan issues: a run of token ids, `begin == last` for a point.
///
/// The cover is the disjunction of these — a row is a candidate as soon as one of
/// its codes falls in one term, which is why a sound cover needs no conjunction:
/// the cut has already put a term on every path an occurrence could take.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct CoverTerm {
    /// First id of the run.
    pub begin: Token,
    /// Last id of the run, inclusive.
    pub last: Token,
    /// Occurrences of the run's tokens in the code stream.
    ///
    /// Terms are disjoint id runs, so these add: the cover's own frequency is
    /// their sum, and the figure can print the addition rather than assert it.
    ///
    /// Over the row count this is the planner's *estimate* of the rows the term
    /// admits, and an upper bound on them — a row holding the token twice counts
    /// twice. [`measured_rows`](Self::measured_rows) is the truth.
    pub term_frequency: u64,
    /// Rows that really hold one of the run's tokens, counted from the column
    /// when the figure was measured.
    pub measured_rows: Option<u64>,
}

/// A cut's membership as OnPair actually probes for it.
#[derive(Clone, Debug)]
pub struct Cover {
    /// The ids probed for — what the kernels compare codes against.
    pub members: Vec<bool>,
    /// Their point/range shape and comparison cost.
    pub shape: CoverShape,
    /// The probes themselves, ascending — the terms of the scan's disjunction.
    pub terms: Vec<CoverTerm>,
}

impl Cover {
    /// Whether the cover names no token id, which settles the query: no row holds
    /// a covered code, so no row matches.
    pub fn is_empty(&self) -> bool {
        self.shape.cmp_cost == 0
    }
}

/// The cover OnPair compiles from a cut's `members`: every maximal run of set ids
/// as one probe.
///
/// # Mirrored from OnPair, by hand
/// One rule of the library is reproduced here — `ProbeCover::from_membership`
/// merging membership into maximal runs — because it is crate-private there, and a
/// figure drawing probes the scan does not issue is a figure that lies.
///
/// Frequencies weight the cut but never change what it selected: stored weights
/// may hold false zeroes, so dropping a probe for looking unused would be unsound.
/// An earlier version of both sides did exactly that, and the figures marked what
/// it dropped; the rule is gone from the planner, so it is gone from here.
pub fn compile_cover(members: &[bool], frequencies: &TokenFrequencyIndex) -> Cover {
    let mut shape = CoverShape::default();
    let mut terms = Vec::new();

    let mut id = 0usize;
    while id < members.len() {
        if !members[id] {
            id += 1;
            continue;
        }
        let begin = id;
        while id < members.len() && members[id] {
            id += 1;
        }
        let last = id - 1;

        shape.member_ids += last - begin + 1;
        if begin == last {
            shape.points += 1;
        } else {
            shape.ranges += 1;
        }
        shape.cmp_cost += 1;
        terms.push(CoverTerm {
            begin: begin as Token,
            last: last as Token,
            term_frequency: (begin..=last)
                .map(|id| u64::from(frequencies.frequency(id as Token)))
                .sum(),
            measured_rows: None,
        });
    }

    Cover {
        members: members.to_vec(),
        shape,
        terms,
    }
}

/// A byte string as the figure shows it: printable ASCII kept, everything else
/// escaped, and nothing longer than `max` bytes shown.
pub(crate) fn preview(bytes: &[u8], max: usize) -> String {
    let mut out = String::new();
    for &byte in bytes.iter().take(max) {
        match byte {
            b' '..=b'~' if !matches!(byte, b'\\' | b'"') => out.push(byte as char),
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            _ => out.push_str(&format!("\\x{byte:02x}")),
        }
    }
    if bytes.len() > max {
        out.push('…');
    }
    out
}

/// The ids of an explicit set, term frequency descending, capped at
/// [`TOKEN_LIST_CAP`].
///
/// Term frequency and not the active metric: it is the count the card prints
/// beside the list, so a reader can check the ordering against the number in
/// front of them.
fn heaviest(set: &ProbeSet, cum_tf: &[u64]) -> Vec<Token> {
    let ProbeSet::Explicit { ids } = set else {
        return Vec::new();
    };
    let mut ranked = ids.clone();
    // Frequency first, then id, so the order is the data's rather than the sort's.
    ranked.sort_by_key(|&id| {
        let index = id as usize;
        (
            std::cmp::Reverse(cum_tf[index + 1] - cum_tf[index]),
            id,
        )
    });
    ranked.truncate(TOKEN_LIST_CAP);
    ranked
}

/// The token greedy parsing takes at the head of `suffix`, and its length.
/// The greedy longest in-needle token at `suffix`, or `None` when no token is a
/// prefix of it.
///
/// `None` is not malformed input: it says no tokenization of any row can have a
/// boundary here with the needle running past it, because a token starting here
/// either ends inside the needle — and then its bytes *are* such a prefix — or
/// covers the rest, which is the terminal range instead. Reading an id off the
/// empty range would probe for an unrelated token, which is what an earlier
/// version of this did.
fn greedy_in_needle<V: DictionaryView>(dict: V, suffix: &[u8]) -> Option<(Token, usize)> {
    for len in (1..=suffix.len().min(MAX_TOKEN_SIZE)).rev() {
        let range = prefix_range(dict, &suffix[..len]);
        if !range.is_empty() && dict.token_len(range.begin) == len {
            return Some((range.begin, len));
        }
    }
    None
}

struct Builder<'a, V: DictionaryView> {
    dict: V,
    needle: &'a [u8],
    cum_tf: &'a [u64],
    cum_df: &'a [u64],
    residual_cum_tf: Vec<u64>,
    residual_cum_df: Vec<u64>,
    edges: Vec<GraphEdge>,
    /// Whether the steps out of an offset have been emitted. Node ids are
    /// offsets, so this is all the builder has to track.
    built: Vec<bool>,
    /// The dictionary's escape code, when it has one.
    escape_token: Option<Token>,
}

impl<'a, V: DictionaryView> Builder<'a, V> {
    fn probe(&self, set: ProbeSet, label: String, detail: String) -> Probe {
        Probe {
            term_frequency: set.prefix_weight(self.cum_tf),
            row_frequency: set.prefix_weight(self.cum_df),
            residual_term_frequency: set.prefix_weight(&self.residual_cum_tf),
            residual_row_frequency: set.prefix_weight(&self.residual_cum_df),
            heaviest: heaviest(&set, self.cum_tf),
            set,
            label,
            detail,
        }
    }

    fn add_edge(&mut self, from: usize, to: usize, kind: EdgeKind, probe: Option<Probe>) {
        let id = self.edges.len();
        self.edges.push(GraphEdge {
            id,
            from,
            to,
            kind,
            probe,
        });
    }

    /// Emit the steps out of `start` and out of every offset its greedy chain
    /// reaches, stopping at the first offset already emitted.
    fn ensure_chain(&mut self, start: usize) {
        let mut offset = start;
        while !self.built[offset] {
            self.built[offset] = true;
            let Some(next) = self.build_state(offset) else {
                break;
            };
            if next >= self.needle.len() {
                break;
            }
            offset = next;
        }
    }

    /// Emit the steps out of the state at `offset`, returning where its greedy
    /// step lands, or `None` when the lane ends here.
    fn build_state(&mut self, offset: usize) -> Option<usize> {
        let end = self.needle.len();
        let suffix = &self.needle[offset..];

        // The occurrence may end inside a longer token starting here.
        let terminal = if suffix.len() > MAX_TOKEN_SIZE {
            TokenRange::EMPTY
        } else {
            prefix_range(self.dict, suffix)
        };
        // Not at offset 0: every token the whole needle is a prefix of also
        // contains it, so the whole-needle step already probes for them. A
        // parallel edge would make the cut pay twice for one path, and draw two
        // cards naming the same token.
        if !terminal.is_empty() && offset != 0 {
            let set = ProbeSet::range(terminal);
            let detail = format!(
                "ids {}..{} ({}), prefix \"{}\"",
                terminal.begin,
                terminal.last,
                terminal.len(),
                preview(suffix, TOKEN_PREVIEW)
            );
            let probe = self.probe(set, format!("terminal range @ {offset}"), detail);
            self.add_edge(offset, end, EdgeKind::Terminal, Some(probe));
        }

        let Some((token, len)) = greedy_in_needle(self.dict, suffix) else {
            // Nothing tokenizes what remains. A dictionary with an escape code
            // answers that with the escape and stops tracing the lane; one without
            // cannot represent the needle at all, so the lane ends here.
            if let Some(escape) = self.escape_token {
                let probe = self.probe(
                    ProbeSet::point(escape),
                    format!("escape @ {offset}"),
                    format!("no token starts \"{}\"", preview(suffix, TOKEN_PREVIEW)),
                );
                self.add_edge(offset, end, EdgeKind::Escape, Some(probe));
            }
            return None;
        };
        let next = offset + len;
        if next < end {
            let text = preview(self.dict.token(token), TOKEN_PREVIEW);
            let probe = self.probe(
                ProbeSet::point(token),
                format!("token #{token} @ {offset}"),
                format!("\"{text}\" → byte {next}"),
            );
            self.add_edge(offset, next, EdgeKind::Step { token }, Some(probe));
        } else {
            // A greedy step reaching the needle's end consumed the rest exactly, so
            // that token is in its own prefix range: the step is already probed by
            // the terminal edge above, and a parallel one would make the cut pay
            // twice for it.
            debug_assert!(
                !terminal.is_empty(),
                "the exact final token belongs to its own prefix range"
            );
        }
        Some(next)
    }
}

/// Dictionary tokens containing the whole needle.
fn contained_tokens<V: DictionaryView>(dict: V, needle: &[u8]) -> Vec<Token> {
    if needle.len() > MAX_TOKEN_SIZE {
        return Vec::new();
    }
    (0..dict.num_tokens())
        .filter(|&id| {
            let token = dict.token(id as Token);
            token.len() >= needle.len()
                && token.windows(needle.len()).any(|window| window == needle)
        })
        .map(|id| id as Token)
        .collect()
}

/// Build the alignment DAG for `needle` over `dict`, weighting each probe by its
/// frequency in `weights`.
///
/// # Errors
/// [`Error::EmptyPattern`] for an empty needle, [`Error::IndexMismatch`] if the
/// weights describe a different dictionary.
pub fn build_path_graph<V: DictionaryView>(
    dict: V,
    needle: &[u8],
    weights: &Weights,
    escape_token: Option<Token>,
) -> Result<PathGraph, Error> {
    if needle.is_empty() {
        return Err(Error::EmptyPattern);
    }
    // Wider than the dictionary is fine and, for a table with escapes, necessary:
    // an escaped byte rides in the code stream as a code of its own. Narrower is
    // an index built for something else.
    if weights.num_tokens() < dict.num_tokens() {
        return Err(Error::IndexMismatch {
            index_tokens: weights.num_tokens(),
            dict_tokens: dict.num_tokens(),
        });
    }

    let contained = contained_tokens(dict, needle);
    // Residual weights: the same frequencies with the whole-needle tokens zeroed,
    // so a metric can price a probe by what it adds to a cover that already holds
    // them — and every cover does, since their step is one a cut has to take.
    let mut mandatory = vec![false; weights.num_tokens()];
    for &id in &contained {
        mandatory[id as usize] = true;
    }
    let residual = |sums: &[u64]| {
        let per_token: Vec<u64> = (0..weights.num_tokens())
            .map(|id| {
                if mandatory[id] { 0 } else { sums[id + 1] - sums[id] }
            })
            .collect();
        cumulative(&per_token)
    };

    let n = needle.len();
    let mut builder = Builder {
        dict,
        needle,
        cum_tf: weights.cum_tf(),
        cum_df: weights.cum_df(),
        residual_cum_tf: residual(weights.cum_tf()),
        residual_cum_df: residual(weights.cum_df()),
        edges: Vec::new(),
        built: vec![false; n],
        escape_token,
    };

    // First-token sets: for each alignment k >= 1, the tokens ending with
    // needle[..k], kept only while there are few enough of them to probe for.
    let kmax = n.min(MAX_TOKEN_SIZE);
    let mut first: Vec<Vec<Token>> = vec![Vec::new(); kmax.max(1)];
    let mut qualifying = vec![0usize; kmax.max(1)];
    for id in 0..dict.num_tokens() {
        let token = dict.token(id as Token);
        let hi = kmax.min(token.len() + 1);
        for k in 1..hi {
            if token[token.len() - k..] == needle[..k] {
                qualifying[k] += 1;
                if first[k].len() < SET_CAP {
                    first[k].push(id as Token);
                }
            }
        }
    }

    // Alignment 0 needs no entry step: nothing precedes its first token, so its
    // layouts begin at the start node itself.
    builder.ensure_chain(0);
    let mut feasible_alignments = 1usize;
    for k in 1..kmax {
        if qualifying[k] == 0 {
            continue;
        }
        feasible_alignments += 1;
        builder.ensure_chain(k);
        let kind = EdgeKind::Entry {
            alignment: k,
            qualifying: qualifying[k],
        };
        let probe = (qualifying[k] <= SET_CAP).then(|| {
            let set = ProbeSet::Explicit {
                ids: first[k].clone(),
            };
            let detail = format!(
                "{} tokens ending with \"{}\"",
                qualifying[k],
                preview(&needle[..k], MAX_TOKEN_SIZE)
            );
            builder.probe(set, format!("first-token set k={k}"), detail)
        });
        builder.add_edge(0, k, kind, probe);
    }

    // A token holding the whole needle needs no boundary: one step, start to
    // accept. It is the only step on that path, so every cut takes it — which is
    // what "mandatory" now means, rather than a rule outside the graph.
    if !contained.is_empty() {
        let set = ProbeSet::Explicit {
            ids: contained.clone(),
        };
        let detail = format!(
            "{} token{} contain \"{}\"",
            contained.len(),
            if contained.len() == 1 { "" } else { "s" },
            preview(needle, TOKEN_PREVIEW)
        );
        let probe = builder.probe(set, "contains the whole needle".to_string(), detail);
        builder.add_edge(
            0,
            n,
            EdgeKind::Contained {
                tokens: contained.len(),
            },
            Some(probe),
        );
    }

    let edges = builder.edges;
    let mut states: Vec<usize> = edges
        .iter()
        .flat_map(|edge| [edge.from, edge.to])
        .collect();
    states.sort_unstable();
    states.dedup();

    let mut incoming = vec![0usize; n + 1];
    let mut outgoing = vec![0usize; n + 1];
    for edge in &edges {
        incoming[edge.to] += 1;
        outgoing[edge.from] += 1;
    }
    let stats = GraphStats {
        feasible_alignments,
        unique_states: states.len(),
        converged_states: states.iter().filter(|&&at| incoming[at] > 1).count(),
        terminal_paths: edges
            .iter()
            .filter(|edge| matches!(edge.kind, EdgeKind::Terminal))
            .count(),
        selectable_probe_edges: edges.iter().filter(|edge| edge.probe.is_some()).count(),
        dead_end_states: states
            .iter()
            .filter(|&&at| at != n && outgoing[at] == 0)
            .count(),
    };

    Ok(PathGraph {
        needle: needle.to_vec(),
        edges,
        states,
        contained,
        escape_token,
        code_space: weights.num_tokens(),
        stats,
        dictionary_size: dict.num_tokens(),
        token_text: (0..weights.num_tokens())
            .map(|id| {
                if id < dict.num_tokens() {
                    preview(dict.token(id as Token), TOKEN_PREVIEW)
                } else if Some(id as Token) == escape_token {
                    "<ESCAPE>".to_string()
                } else {
                    // A code past the dictionary that is not the escape marker is
                    // the literal byte an escape carried.
                    format!("<byte {id:#04x}>")
                }
            })
            .collect(),
        total_codes: weights.total_codes(),
        total_rows: weights.total_rows,
        column_rows: weights.total_rows,
    })
}
