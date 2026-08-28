//! Minimum weighted cut of the alignment DAG, by max-flow.
//!
//! Probes ride on the edges, so the cheapest sound cover is a minimum cut of the
//! edge weights — max-flow directly, with no node splitting. A step no cut may
//! select gets a capacity larger than any finite cut, which keeps the minimum cut
//! on the edges where it means something.
//!
//! The point of cutting the *merged* DAG rather than choosing per alignment is
//! that alignments converging on a shared suffix are paid for once, at the join,
//! instead of once each.

use std::collections::VecDeque;

use serde::Serialize;

use crate::graph::{PathGraph, ProbeWeight};

#[derive(Clone, Debug)]
struct Edge {
    to: usize,
    rev: usize,
    cap: u64,
}

#[derive(Debug)]
struct Dinic {
    adj: Vec<Vec<Edge>>,
    level: Vec<i32>,
    next: Vec<usize>,
}

impl Dinic {
    fn new(n: usize) -> Self {
        Self {
            adj: vec![Vec::new(); n],
            level: vec![-1; n],
            next: vec![0; n],
        }
    }

    fn add_edge(&mut self, from: usize, to: usize, cap: u64) {
        let fwd = Edge {
            to,
            rev: self.adj[to].len(),
            cap,
        };
        let rev = Edge {
            to: from,
            rev: self.adj[from].len(),
            cap: 0,
        };
        self.adj[from].push(fwd);
        self.adj[to].push(rev);
    }

    fn build_levels(&mut self, source: usize, sink: usize) -> bool {
        self.level.fill(-1);
        self.level[source] = 0;
        let mut queue = VecDeque::from([source]);
        while let Some(v) = queue.pop_front() {
            for edge in &self.adj[v] {
                if edge.cap > 0 && self.level[edge.to] < 0 {
                    self.level[edge.to] = self.level[v] + 1;
                    queue.push_back(edge.to);
                }
            }
        }
        self.level[sink] >= 0
    }

    fn send(&mut self, v: usize, sink: usize, pushed: u64) -> u64 {
        if v == sink || pushed == 0 {
            return pushed;
        }
        while self.next[v] < self.adj[v].len() {
            let edge_idx = self.next[v];
            let edge = self.adj[v][edge_idx].clone();
            if edge.cap > 0 && self.level[edge.to] == self.level[v] + 1 {
                let sent = self.send(edge.to, sink, pushed.min(edge.cap));
                if sent > 0 {
                    self.adj[v][edge_idx].cap -= sent;
                    self.adj[edge.to][edge.rev].cap += sent;
                    return sent;
                }
            }
            self.next[v] += 1;
        }
        0
    }

    fn max_flow(&mut self, source: usize, sink: usize) -> u64 {
        let mut total = 0u64;
        while self.build_levels(source, sink) {
            self.next.fill(0);
            loop {
                let sent = self.send(source, sink, u64::MAX);
                if sent == 0 {
                    break;
                }
                total = total.checked_add(sent).expect("max-flow capacity overflow");
            }
        }
        total
    }

    fn reachable(&self, source: usize) -> Vec<bool> {
        let mut seen = vec![false; self.adj.len()];
        seen[source] = true;
        let mut queue = VecDeque::from([source]);
        while let Some(v) = queue.pop_front() {
            for edge in &self.adj[v] {
                if edge.cap > 0 && !seen[edge.to] {
                    seen[edge.to] = true;
                    queue.push_back(edge.to);
                }
            }
        }
        seen
    }
}

/// A minimum cut: its total weight and the steps it selects.
#[derive(Clone, Debug, Serialize)]
pub struct CutResult {
    /// Sum of the selected probes' weights under the chosen metric.
    pub value: u64,
    /// Ids of the selected edges, ascending.
    pub selected_edges: Vec<usize>,
}

/// Solve the weighted minimum cut.
///
/// # Panics
/// If every cut of some start-to-accept path is infinite, which would mean the
/// DAG admits a match no cover can catch. The builder cannot produce such a path:
/// a lane either ends at a terminal step, which carries a probe, or dies before
/// reaching the accept position at all.
pub fn minimum_cut(graph: &PathGraph, weights: ProbeWeight) -> CutResult {
    let capacities: Vec<Option<u64>> = graph
        .edges
        .iter()
        .map(|edge| graph.edge_capacity(edge, weights))
        .collect();
    let finite_sum = capacities
        .iter()
        .flatten()
        .try_fold(0u64, |acc, &cap| acc.checked_add(cap))
        .expect("sum of probe capacities overflowed u64");
    let infinite = finite_sum.saturating_add(1);

    let nodes = graph.end() + 1;
    let mut flow = Dinic::new(nodes);
    // Arc i of the flow graph is edge i of the DAG, so a saturated arc names the
    // step it came from without a lookup table.
    for (edge, &cap) in graph.edges.iter().zip(&capacities) {
        flow.add_edge(edge.from, edge.to, cap.unwrap_or(infinite));
    }

    let value = flow.max_flow(0, graph.end());
    assert!(
        value < infinite,
        "graph contains an uncuttable start/accept path"
    );
    let reachable = flow.reachable(0);
    let selected_edges = graph
        .edges
        .iter()
        .zip(&capacities)
        .filter(|(edge, cap)| cap.is_some() && reachable[edge.from] && !reachable[edge.to])
        .map(|(edge, _)| edge.id)
        .collect();

    CutResult {
        value,
        selected_edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeKind, GraphEdge, PathGraph, Probe, ProbeSet};

    /// One step, cuttable at `cap` or uncuttable when `cap` is `None`.
    fn step(id: usize, from: usize, to: usize, cap: Option<u64>) -> GraphEdge {
        GraphEdge {
            id,
            from,
            to,
            kind: EdgeKind::Step { token: 0 },
            probe: cap.map(|tf| Probe {
                set: ProbeSet::Point { id: id as u16 },
                term_frequency: tf,
                row_frequency: tf,
                residual_term_frequency: tf,
                residual_row_frequency: tf,
                label: String::new(),
                detail: String::new(),
                heaviest: Vec::new(),
            }),
        }
    }

    fn graph(needle_len: usize, edges: Vec<GraphEdge>) -> PathGraph {
        PathGraph {
            needle: vec![b'x'; needle_len],
            edges,
            states: (0..=needle_len).collect(),
            contained: Vec::new(),
            escape_token: None,
            code_space: 8,
            stats: Default::default(),
            dictionary_size: 8,
            total_codes: 10,
            total_rows: 10,
            column_rows: 10,
            token_text: Vec::new(),
        }
    }

    /// The reason a global cut beats per-alignment local choices: two lanes that
    /// converge are blocked once at the join, not twice at their own terminals.
    #[test]
    fn shared_suffix_beats_two_local_choices() {
        // 0 -> 1 -(4)-> 3 -(6)-> 4  and  0 -> 2 -(4)-> 3
        let cut = minimum_cut(
            &graph(
                4,
                vec![
                    step(0, 0, 1, None),
                    step(1, 0, 2, None),
                    step(2, 1, 3, Some(4)),
                    step(3, 2, 3, Some(4)),
                    step(4, 3, 4, Some(6)),
                ],
            ),
            ProbeWeight::TermFrequency,
        );
        assert_eq!(cut.value, 6);
        assert_eq!(cut.selected_edges, vec![4]);
    }

    /// An uncuttable step is paid for further along its chain, not skipped.
    #[test]
    fn an_unprobed_step_pushes_the_cut_downstream() {
        let cut = minimum_cut(
            &graph(
                2,
                vec![step(0, 0, 1, None), step(1, 1, 2, Some(3))],
            ),
            ProbeWeight::TermFrequency,
        );
        assert_eq!(cut.value, 3);
        assert_eq!(cut.selected_edges, vec![1]);
    }
}
