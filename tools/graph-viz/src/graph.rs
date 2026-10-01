//! The alignment DAG as the figure draws it, built from a plan log.
//!
//! Nodes are needle offsets: `0` is the start and the needle length the accept
//! position. Every edge is one parse step carrying the probe a scan looks for to
//! catch it, and the planner's cut is a set of such edges. The log says what each
//! step probes; this module names the tokens, weighs the probes and classifies
//! the steps for the renderer.

use crate::load::{Cut, Label, LogEdge, Query, Symbols};

/// A code id: OnPair's `u16` token or an FSST byte code.
pub type Token = u16;

/// Bytes of a token the figure spells out: OnPair's longest token, so no token
/// is ever shown cut.
pub(crate) const TOKEN_PREVIEW: usize = 16;

/// Ids of one probe the figure names, heaviest first. The rest become an "and N
/// more" tail.
const TOKEN_LIST_CAP: usize = 10;

/// The token ids one probe tests for.
#[derive(Clone, Debug)]
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
    /// An enumerated set, scattered through the dictionary.
    Explicit {
        /// The ids, ascending.
        ids: Vec<Token>,
    },
}

impl ProbeSet {
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

    fn term_frequency(&self, frequency: &[u64]) -> u64 {
        let mut sum = 0;
        self.for_each(|id| sum += frequency.get(id as usize).copied().unwrap_or(0));
        sum
    }
}

/// One testable set of token ids, weighted and labelled for drawing.
#[derive(Clone, Debug)]
pub struct Probe {
    /// The ids this probe tests for.
    pub set: ProbeSet,
    /// Token occurrences in the code stream.
    pub term_frequency: u64,
    /// Short label, drawn on the card.
    pub label: String,
    /// Longer explanation, drawn under the label and used as the SVG tooltip.
    pub detail: String,
    /// Up to [`TOKEN_LIST_CAP`] of an explicit set's ids, term frequency
    /// descending, for the figure to name. Empty for points and ranges.
    pub heaviest: Vec<Token>,
}

/// What a parse step is.
#[derive(Clone, Copy, Debug)]
pub enum EdgeKind {
    /// Entering alignment `k`: the occurrence's first token ends with
    /// `needle[..k]`.
    Entry {
        /// The alignment's `k`.
        alignment: usize,
        /// Tokens that qualify as that first token, reported even when the step
        /// carries no probe because there were too many.
        qualifying: usize,
    },
    /// One code covering `needle[from..to]` short of the accept position: a
    /// greedy interior token, or FSST's escape code and the literal byte after it.
    Step {
        /// That code.
        token: Token,
    },
    /// Tokens the remaining needle is a prefix of: reaching one is a match.
    Terminal,
    /// Tokens holding the whole needle after their first byte: one step, start
    /// to accept.
    Contained {
        /// How many tokens hold it.
        tokens: usize,
    },
    /// FSST's escape code carrying the needle's last byte: an accepting step.
    Escape,
}

/// One parse step, and the probe that catches every layout crossing it.
#[derive(Clone, Debug)]
pub struct GraphEdge {
    /// Index of this edge in [`PathGraph::edges`].
    pub id: usize,
    /// Needle offset the step leaves; `0` is the start.
    pub from: usize,
    /// Needle offset it reaches; `needle.len()` is the accept position.
    pub to: usize,
    /// Which shape it is.
    pub kind: EdgeKind,
    /// The set a scan looks for, or `None` on a step no cut may select.
    pub probe: Option<Probe>,
}

/// Shape counters worth reporting next to a figure.
#[derive(Clone, Debug, Default)]
pub struct GraphStats {
    /// Alignments a layout can enter through: `k = 0` plus every entry step.
    pub feasible_alignments: usize,
    /// Offsets a parse step leaves or reaches.
    pub unique_states: usize,
    /// States more than one step reaches.
    pub converged_states: usize,
    /// Steps into the accept position.
    pub terminal_paths: usize,
    /// Edges a cut may select.
    pub selectable_probe_edges: usize,
    /// States with no step out that are not the accept position.
    pub dead_end_states: usize,
}

/// The alignment DAG for one needle over one row group's dictionary.
#[derive(Clone, Debug)]
pub struct PathGraph {
    /// The needle this graph was built for.
    pub needle: Vec<u8>,
    /// Every parse step.
    pub edges: Vec<GraphEdge>,
    /// Offsets any step leaves or reaches, ascending.
    pub states: Vec<usize>,
    /// Token ids holding the whole needle away from their start.
    pub contained: Vec<Token>,
    /// Ids a code can take: one past the largest id in the symbol table.
    pub code_space: usize,
    /// Shape counters.
    pub stats: GraphStats,
    /// Total code occurrences over the symbol table.
    pub total_codes: u64,
    /// Rows in the row group.
    pub column_rows: u64,
    /// Every id, previewed and escaped, so the drawing can name a probe by what
    /// it matches.
    pub token_text: Vec<String>,
}

impl PathGraph {
    /// The accept position: every needle byte consumed.
    pub fn end(&self) -> usize {
        self.needle.len()
    }

    /// The token ids a cut selects: the union of the selected edges' sets.
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

/// A logged query's graph, with the edges its cut takes outright and the edges
/// it takes only as the second code of a pair.
#[derive(Clone, Debug)]
pub struct LoggedGraph {
    /// The graph.
    pub graph: PathGraph,
    /// Edges the cut takes on their own.
    pub cut: Vec<usize>,
    /// Edges the cut takes paired with the code before them.
    pub paired: Vec<usize>,
    /// Those pairs as (code before, the edge's code), ascending.
    pub pairs: Vec<(Token, Token)>,
}

/// Build the drawable graph for `query` from its logged edges and symbol table.
///
/// # Errors
/// An edge whose endpoints do not fit its label: a set that is neither an entry
/// out of the start nor the whole-needle step, or a range not ending at accept.
pub fn from_log(query: &Query, edges: &[LogEdge], symbols: &Symbols) -> Result<LoggedGraph, String> {
    let needle = &query.needle;
    let n = needle.len();
    let text = |id: Token| match symbols.content.get(id as usize) {
        Some(Some(bytes)) if bytes.is_empty() => "<ESC>".to_string(),
        Some(Some(bytes)) => preview(bytes, TOKEN_PREVIEW),
        _ => format!("<code {id}>"),
    };
    let weigh = |set: ProbeSet, label: String, detail: String| Probe {
        term_frequency: set.term_frequency(&symbols.frequency),
        heaviest: heaviest(&set, &symbols.frequency),
        set,
        label,
        detail,
    };
    // Tokens ending with needle[..k] and longer than k, for an entry whose set
    // was too big to log.
    let qualifying = |k: usize| {
        symbols
            .content
            .iter()
            .flatten()
            .filter(|t| t.len() > k && t.ends_with(&needle[..k]))
            .count()
    };

    let mut graph_edges = Vec::with_capacity(edges.len());
    let (mut cut, mut paired, mut pairs, mut contained) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (id, edge) in edges.iter().enumerate() {
        let (from, to) = (edge.from, edge.to);
        if from >= to || to > n {
            return Err(format!("edge {from}->{to} outside a {n}-byte needle"));
        }
        let (kind, probe) = match &edge.label {
            Label::Code(token) if to < n => (
                EdgeKind::Step { token: *token },
                Some(weigh(
                    ProbeSet::Point { id: *token },
                    format!("token #{token} @ {from}"),
                    format!("\"{}\" → byte {to}", text(*token)),
                )),
            ),
            Label::EscapeCode(byte) => {
                let escape = symbols.escape().ok_or("escape edge without an escape code")?;
                (
                    if to == n {
                        EdgeKind::Escape
                    } else {
                        EdgeKind::Step { token: escape }
                    },
                    Some(weigh(
                        ProbeSet::Point { id: escape },
                        format!("escape @ {from}"),
                        format!("<ESC>{} → byte {to}", preview(&[*byte], 1)),
                    )),
                )
            }
            Label::CodeRange(lo, hi) if to == n => (
                EdgeKind::Terminal,
                Some(weigh(
                    ProbeSet::Range { lo: *lo, hi: *hi },
                    format!("terminal range @ {from}"),
                    format!(
                        "ids {lo}..{hi} ({}), prefix \"{}\"",
                        hi - lo + 1,
                        preview(&needle[from..], TOKEN_PREVIEW)
                    ),
                )),
            ),
            Label::CodeSet(ids) if from == 0 && to == n => {
                contained.extend_from_slice(ids);
                (
                    EdgeKind::Contained { tokens: ids.len() },
                    Some(weigh(
                        ProbeSet::Explicit { ids: ids.clone() },
                        "contains the whole needle".to_string(),
                        format!(
                            "{} token{} contain \"{}\"",
                            ids.len(),
                            if ids.len() == 1 { "" } else { "s" },
                            preview(needle, TOKEN_PREVIEW)
                        ),
                    )),
                )
            }
            Label::CodeSet(ids) if from == 0 => (
                EdgeKind::Entry {
                    alignment: to,
                    qualifying: ids.len(),
                },
                Some(weigh(
                    ProbeSet::Explicit { ids: ids.clone() },
                    format!("first-token set k={to}"),
                    format!(
                        "{} tokens ending with \"{}\"",
                        ids.len(),
                        preview(&needle[..to], TOKEN_PREVIEW)
                    ),
                )),
            ),
            Label::None if from == 0 => (
                EdgeKind::Entry {
                    alignment: to,
                    qualifying: qualifying(to),
                },
                None,
            ),
            other => return Err(format!("{other:?} does not fit edge {from}->{to}")),
        };
        match &edge.cut {
            Cut::No => {}
            Cut::Yes => cut.push(id),
            Cut::Paired(before) => {
                let second = match edge.label {
                    Label::Code(token) => token,
                    Label::CodeRange(lo, hi) if lo == hi => lo,
                    _ => return Err(format!("edge {from}->{to} is paired but not one code")),
                };
                paired.push(id);
                pairs.extend(before.iter().map(|&first| (first, second)));
            }
        }
        graph_edges.push(GraphEdge {
            id,
            from,
            to,
            kind,
            probe,
        });
    }

    let mut states: Vec<usize> = graph_edges.iter().flat_map(|e| [e.from, e.to]).collect();
    states.sort_unstable();
    states.dedup();
    let mut incoming = vec![0usize; n + 1];
    let mut outgoing = vec![0usize; n + 1];
    for edge in &graph_edges {
        incoming[edge.to] += 1;
        outgoing[edge.from] += 1;
    }
    let stats = GraphStats {
        feasible_alignments: 1 + graph_edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Entry { .. }))
            .count(),
        unique_states: states.len(),
        converged_states: states.iter().filter(|&&at| incoming[at] > 1).count(),
        terminal_paths: graph_edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Terminal))
            .count(),
        selectable_probe_edges: graph_edges.iter().filter(|e| e.probe.is_some()).count(),
        dead_end_states: states
            .iter()
            .filter(|&&at| at != n && outgoing[at] == 0)
            .count(),
    };

    let code_space = symbols.content.len();
    Ok(LoggedGraph {
        graph: PathGraph {
            needle: needle.clone(),
            edges: graph_edges,
            states,
            contained,
            code_space,
            stats,
            total_codes: symbols.frequency.iter().sum(),
            column_rows: query.rows,
            token_text: (0..code_space).map(|id| text(id as Token)).collect(),
        },
        cut,
        paired,
        pairs: {
            pairs.sort_unstable();
            pairs.dedup();
            pairs
        },
    })
}

/// How a membership table looks once expressed as points and ranges.
#[derive(Clone, Copy, Debug, Default)]
pub struct CoverShape {
    /// Token ids probed for.
    pub member_ids: usize,
    /// Runs of length one.
    pub points: usize,
    /// Runs of length two or more.
    pub ranges: usize,
    /// SIMD comparisons the cover costs: one per run.
    pub cmp_cost: usize,
}

/// One probe the scan issues: a run of token ids, `begin == last` for a point.
#[derive(Clone, Copy, Debug)]
pub struct CoverTerm {
    /// First id of the run.
    pub begin: Token,
    /// Last id of the run, inclusive.
    pub last: Token,
    /// Occurrences of the run's tokens in the code stream. Over the row count
    /// this is the planner's estimate of the rows the term admits, and an upper
    /// bound on them.
    pub term_frequency: u64,
}

/// A cut's membership as the scan probes for it.
#[derive(Clone, Debug)]
pub struct Cover {
    /// The ids probed for.
    pub members: Vec<bool>,
    /// Their point/range shape and comparison cost.
    pub shape: CoverShape,
    /// The probes themselves, ascending: the terms of the scan's disjunction.
    pub terms: Vec<CoverTerm>,
}

/// The cover a cut's `members` compile to: every maximal run of set ids as one
/// probe, which is how both planners merge a cut's probes.
pub fn compile_cover(members: &[bool], frequency: &[u64]) -> Cover {
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
            term_frequency: frequency[begin..=last].iter().sum(),
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
fn heaviest(set: &ProbeSet, frequency: &[u64]) -> Vec<Token> {
    let ProbeSet::Explicit { ids } = set else {
        return Vec::new();
    };
    let mut ranked = ids.clone();
    // Frequency first, then id, so the order is the data's rather than the sort's.
    ranked.sort_by_key(|&id| {
        (
            std::cmp::Reverse(frequency.get(id as usize).copied().unwrap_or(0)),
            id,
        )
    });
    ranked.truncate(TOKEN_LIST_CAP);
    ranked
}
