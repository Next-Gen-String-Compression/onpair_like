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

use std::collections::{BTreeMap, HashMap};

use onpair::search::{TokenFrequencyIndex, prefix_range};
use onpair::{ColumnView, DictionaryView, MAX_TOKEN_SIZE, Offset, Token, TokenRange};
use serde::Serialize;

use crate::Error;

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

/// What a node stands for, and where the renderer should put it.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeKind {
    /// Single entry node, upstream of every alignment.
    Source,
    /// Single accepting node.
    Sink,
    /// Entry point of one feasible alignment.
    Junction {
        /// Human-readable reason this alignment exists, or why it is unprobed.
        label: String,
        /// The alignment's `k`, i.e. how many needle bytes the first token holds.
        offset: Option<usize>,
    },
    /// Parsing has consumed the needle up to this byte offset.
    State {
        /// Needle byte offset.
        offset: usize,
    },
    /// The one interior token greedy parsing takes from `offset`.
    Point {
        /// Needle byte offset the token starts at.
        offset: usize,
        /// Byte offset after it.
        next_offset: usize,
    },
    /// Tokens whose prefix is the whole remaining needle: reaching one is a match.
    TerminalRange {
        /// Needle byte offset the remainder starts at.
        offset: usize,
    },
    /// The tokens ending with `needle[..alignment]`, entering that alignment.
    FirstSet {
        /// The alignment's `k`.
        alignment: usize,
    },
}

/// A DAG node.
#[derive(Clone, Debug, Serialize)]
pub struct GraphNode {
    /// Index of this node in [`PathGraph::nodes`].
    pub id: usize,
    /// What it stands for.
    pub kind: NodeKind,
    /// Present exactly on cuttable nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<Probe>,
}

/// A directed edge.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct GraphEdge {
    /// Tail node id.
    pub from: usize,
    /// Head node id.
    pub to: usize,
}

/// Shape counters worth reporting next to a figure.
#[derive(Clone, Debug, Default, Serialize)]
pub struct GraphStats {
    /// Alignments with at least one dictionary token ending in that needle prefix.
    pub feasible_alignments: usize,
    /// Distinct (alignment, accepting terminal) pairs — paths a match can take.
    pub terminal_paths: usize,
    /// States each alignment would visit if lanes were kept separate.
    pub state_visits_before_merge: usize,
    /// States actually materialized, after merging by byte offset.
    pub unique_states: usize,
    /// Materialized states reached by more than one alignment.
    pub converged_states: usize,
    /// Visits saved by merging: `state_visits_before_merge - unique_states`.
    pub convergence_savings: usize,
    /// Alignments sharing the busiest state.
    pub max_state_uses: usize,
    /// Nodes a cut may select.
    pub selectable_probe_nodes: usize,
}

/// The alignment DAG for one needle over one dictionary.
#[derive(Clone, Debug, Serialize)]
pub struct PathGraph {
    /// The needle this graph was built for.
    pub needle: Vec<u8>,
    /// All nodes, indexed by [`GraphNode::id`].
    pub nodes: Vec<GraphNode>,
    /// All edges.
    pub edges: Vec<GraphEdge>,
    /// Id of the [`Source`](NodeKind::Source) node.
    pub source: usize,
    /// Id of the [`Sink`](NodeKind::Sink) node.
    pub sink: usize,
    /// Token ids containing the whole needle. Mandatory members of every cover,
    /// and deliberately outside the DAG: such a token matches without crossing a
    /// boundary, so no path stands for it and no cut could select it.
    pub contained: Vec<Token>,
    /// Shape counters.
    pub stats: GraphStats,
    /// Dictionary token count.
    pub dictionary_size: usize,
    /// Total token occurrences in the code stream the weights came from.
    pub total_codes: u64,
    /// Row count, or zero when the weights came from an index alone.
    pub total_rows: u64,
}

impl PathGraph {
    /// Flow capacity of a node: its probe weight, or zero for structural nodes.
    pub fn node_capacity(&self, node: &GraphNode, metric: ProbeWeight) -> u64 {
        node.probe.as_ref().map_or(0, |probe| probe.weight(metric))
    }

    /// The token ids a cut selects, plus the mandatory contained ones.
    ///
    /// # Panics
    /// If `selected_nodes` names a structural node.
    pub fn membership_for_cut(&self, selected_nodes: &[usize]) -> Vec<bool> {
        let mut members = vec![false; self.dictionary_size];
        for &id in &self.contained {
            members[id as usize] = true;
        }
        for &node_id in selected_nodes {
            self.nodes[node_id]
                .probe
                .as_ref()
                .expect("cut selected a non-probe node")
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
    /// Token ids in the cover.
    pub member_ids: usize,
    /// Runs of length one.
    pub points: usize,
    /// Runs of length two or more.
    pub ranges: usize,
    /// SIMD comparisons the cover costs: one per point, two per range.
    pub cmp_cost: usize,
}

/// Best exact point/range representation of a membership table when a range
/// costs two SIMD comparisons. Runs of length two become one range: same compare
/// cost, fewer logical probes and constants.
pub fn normalized_shape(members: &[bool]) -> CoverShape {
    let mut shape = CoverShape {
        member_ids: members.iter().filter(|&&member| member).count(),
        ..CoverShape::default()
    };
    let mut i = 0usize;
    while i < members.len() {
        if !members[i] {
            i += 1;
            continue;
        }
        let begin = i;
        while i + 1 < members.len() && members[i + 1] {
            i += 1;
        }
        if i == begin {
            shape.points += 1;
            shape.cmp_cost += 1;
        } else {
            shape.ranges += 1;
            shape.cmp_cost += 2;
        }
        i += 1;
    }
    shape
}

fn preview(bytes: &[u8], max: usize) -> String {
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

/// The token greedy parsing takes at the head of `suffix`, and its length.
fn greedy_in_needle<V: DictionaryView>(dict: V, suffix: &[u8]) -> (Token, usize) {
    let max_len = suffix.len().min(MAX_TOKEN_SIZE);
    for len in (1..=max_len).rev() {
        let range = prefix_range(dict, &suffix[..len]);
        if !range.is_empty() && dict.token_len(range.begin) == len {
            return (range.begin, len);
        }
    }
    let range = prefix_range(dict, &suffix[..1]);
    (range.begin, 1)
}

struct Builder<'a, V: DictionaryView> {
    dict: V,
    needle: &'a [u8],
    cum_tf: &'a [u64],
    cum_df: &'a [u64],
    residual_cum_tf: Vec<u64>,
    residual_cum_df: Vec<u64>,
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    source: usize,
    sink: usize,
    state_nodes: BTreeMap<usize, usize>,
    range_nodes: HashMap<usize, usize>,
}

impl<'a, V: DictionaryView> Builder<'a, V> {
    fn add_node(&mut self, kind: NodeKind, probe: Option<Probe>) -> usize {
        let id = self.nodes.len();
        self.nodes.push(GraphNode { id, kind, probe });
        id
    }

    fn add_edge(&mut self, from: usize, to: usize) {
        self.edges.push(GraphEdge { from, to });
    }

    fn probe(&self, set: ProbeSet, label: String, detail: String) -> Probe {
        Probe {
            term_frequency: set.prefix_weight(self.cum_tf),
            row_frequency: set.prefix_weight(self.cum_df),
            residual_term_frequency: set.prefix_weight(&self.residual_cum_tf),
            residual_row_frequency: set.prefix_weight(&self.residual_cum_df),
            set,
            label,
            detail,
        }
    }

    /// Materialize the state at `offset` and everything downstream of it, once.
    /// Re-entering an existing offset is what merges two alignments into one lane.
    fn ensure_state(&mut self, offset: usize) -> usize {
        if let Some(&node) = self.state_nodes.get(&offset) {
            return node;
        }

        let state = self.add_node(NodeKind::State { offset }, None);
        self.state_nodes.insert(offset, state);
        let suffix = &self.needle[offset..];
        let terminal = prefix_range(self.dict, suffix);
        if !terminal.is_empty() {
            let set = ProbeSet::range(terminal);
            let label = format!("terminal range @ {offset}");
            let detail = format!(
                "ids {}..{} ({}), prefix \"{}\"",
                terminal.begin,
                terminal.last,
                terminal.len(),
                preview(suffix, 12)
            );
            let probe = self.probe(set, label, detail);
            let range_node = self.add_node(NodeKind::TerminalRange { offset }, Some(probe));
            self.range_nodes.insert(offset, range_node);
            self.add_edge(state, range_node);
            self.add_edge(range_node, self.sink);
        }

        let (token, len) = greedy_in_needle(self.dict, suffix);
        let next_offset = offset + len;
        if next_offset < self.needle.len() {
            let tok = self.dict.token(token);
            let set = ProbeSet::point(token);
            let label = format!("token #{token} @ {offset}");
            let detail = format!("\"{}\" → byte {next_offset}", preview(tok, 12));
            let probe = self.probe(set, label, detail);
            let point_node = self.add_node(
                NodeKind::Point {
                    offset,
                    next_offset,
                },
                Some(probe),
            );
            let next_state = self.ensure_state(next_offset);
            self.add_edge(state, point_node);
            self.add_edge(point_node, next_state);
        } else {
            assert!(
                !terminal.is_empty(),
                "the exact final token must belong to its prefix range"
            );
        }
        state
    }
}

/// Dictionary tokens containing the whole needle.
fn contained_tokens<V: DictionaryView>(dict: V, needle: &[u8]) -> Vec<Token> {
    if needle.len() > MAX_TOKEN_SIZE {
        return Vec::new();
    }
    (0..dict.num_tokens())
        .filter_map(|id| {
            let token = dict.token(id as Token);
            (token.len() >= needle.len()
                && token.windows(needle.len()).any(|window| window == needle))
            .then_some(id as Token)
        })
        .collect()
}

/// Build the alignment DAG for `needle`.
///
/// States merge exactly when they begin at the same needle byte offset, which is
/// sound because the remaining parsing problem is then identical.
///
/// # Errors
/// [`Error::EmptyPattern`] for an empty needle, [`Error::IndexMismatch`] if the
/// weights were built for a different dictionary size.
pub fn build_path_graph<V: DictionaryView>(
    dict: V,
    needle: &[u8],
    weights: &Weights,
) -> Result<PathGraph, Error> {
    if needle.is_empty() {
        return Err(Error::EmptyPattern);
    }
    if weights.num_tokens() != dict.num_tokens() {
        return Err(Error::IndexMismatch {
            index_tokens: weights.num_tokens(),
            dict_tokens: dict.num_tokens(),
        });
    }

    let contained = contained_tokens(dict, needle);
    let mut mandatory = vec![false; dict.num_tokens()];
    for &id in &contained {
        mandatory[id as usize] = true;
    }
    let residual = |cum: &[u64]| -> Vec<u64> {
        let per_token: Vec<u64> = (0..dict.num_tokens())
            .map(|id| {
                if mandatory[id] {
                    0
                } else {
                    cum[id + 1] - cum[id]
                }
            })
            .collect();
        cumulative(&per_token)
    };

    let mut builder = Builder {
        dict,
        needle,
        cum_tf: &weights.cum_tf,
        cum_df: &weights.cum_df,
        residual_cum_tf: residual(&weights.cum_tf),
        residual_cum_df: residual(&weights.cum_df),
        nodes: Vec::new(),
        edges: Vec::new(),
        source: 0,
        sink: 0,
        state_nodes: BTreeMap::new(),
        range_nodes: HashMap::new(),
    };

    builder.source = builder.add_node(NodeKind::Source, None);
    builder.sink = builder.add_node(NodeKind::Sink, None);

    // Pass A: which alignments are feasible, and the tokens that enter them.
    // `k` bytes of the needle sit at the tail of the first token, so k=0 is the
    // token-boundary case and always feasible.
    let kmax = needle.len().min(MAX_TOKEN_SIZE);
    let mut feasible = vec![false; kmax];
    let mut first_count = vec![0usize; kmax];
    let mut first_members = vec![Vec::<Token>::new(); kmax];
    feasible[0] = true;
    for id in 0..dict.num_tokens() {
        let token = dict.token(id as Token);
        let hi = kmax.min(token.len() + 1);
        for k in 1..hi {
            if token[token.len() - k..] == needle[..k] {
                first_count[k] += 1;
                if first_count[k] <= SET_CAP {
                    first_members[k].push(id as Token);
                }
            }
        }
    }
    for k in 1..kmax {
        feasible[k] = first_count[k] > 0;
    }

    // Pass B: one lane per feasible alignment, merging on shared byte offsets.
    let mut state_uses: HashMap<usize, usize> = HashMap::new();
    for k in 0..kmax {
        if !feasible[k] {
            continue;
        }
        let alignment = builder.add_node(
            NodeKind::Junction {
                label: if k == 0 {
                    "alignment k=0 (token boundary)".to_string()
                } else if first_count[k] <= SET_CAP {
                    format!("alignment k={k}")
                } else {
                    format!("alignment k={k}; first set {} > cap", first_count[k])
                },
                offset: Some(k),
            },
            None,
        );
        builder.add_edge(builder.source, alignment);
        let state = builder.ensure_state(k);
        if k >= 1 && first_count[k] <= SET_CAP {
            let ids = first_members[k].clone();
            let set = ProbeSet::Explicit { ids };
            let label = format!("first-token set k={k}");
            let detail = format!(
                "{} ids ending with \"{}\"",
                first_count[k],
                preview(&needle[..k], 12)
            );
            let probe = builder.probe(set, label, detail);
            let node = builder.add_node(NodeKind::FirstSet { alignment: k }, Some(probe));
            builder.add_edge(alignment, node);
            builder.add_edge(node, state);
        } else {
            // Above the cap the entry goes unprobed: the cut must block this
            // alignment somewhere down its lane instead.
            builder.add_edge(alignment, state);
        }

        // Walk the lane once for the convergence counters and the terminal count.
        let mut p = k;
        loop {
            *state_uses.entry(p).or_insert(0) += 1;
            let (_, len) = greedy_in_needle(dict, &needle[p..]);
            p += len;
            if p == needle.len() {
                break;
            }
        }
    }

    let mut terminal_paths = 0usize;
    for (k, &ok) in feasible.iter().enumerate() {
        if !ok {
            continue;
        }
        let mut p = k;
        loop {
            if builder.range_nodes.contains_key(&p) {
                terminal_paths += 1;
            }
            let (_, len) = greedy_in_needle(dict, &needle[p..]);
            p += len;
            if p == needle.len() {
                break;
            }
        }
    }

    let state_visits_before_merge: usize = state_uses.values().sum();
    let unique_states = state_uses.len();
    let stats = GraphStats {
        feasible_alignments: feasible.iter().filter(|&&value| value).count(),
        terminal_paths,
        state_visits_before_merge,
        unique_states,
        converged_states: state_uses.values().filter(|&&uses| uses > 1).count(),
        convergence_savings: state_visits_before_merge.saturating_sub(unique_states),
        max_state_uses: state_uses.values().copied().max().unwrap_or(0),
        selectable_probe_nodes: builder
            .nodes
            .iter()
            .filter(|node| node.probe.is_some())
            .count(),
    };

    Ok(PathGraph {
        needle: needle.to_vec(),
        nodes: builder.nodes,
        edges: builder.edges,
        source: builder.source,
        sink: builder.sink,
        contained,
        stats,
        dictionary_size: dict.num_tokens(),
        total_codes: weights.total_codes(),
        total_rows: weights.total_rows,
    })
}
