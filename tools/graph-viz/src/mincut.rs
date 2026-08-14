//! Minimum weighted vertex cut of the alignment DAG, by max-flow.
//!
//! Every node is split into an `in → out` edge whose capacity is the node's
//! weight; probe nodes get their frequency, structural nodes and the DAG's own
//! edges get a capacity larger than any finite cut. A minimum s-t edge cut of
//! that graph can therefore only consist of split edges, i.e. it names a set of
//! probes — and by max-flow/min-cut it is the cheapest set that intersects every
//! source-to-sink path.

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

/// A minimum cut: its total weight and the probe nodes it selects.
#[derive(Clone, Debug, Serialize)]
pub struct CutResult {
    /// Sum of the selected probes' weights under the chosen metric.
    pub value: u64,
    /// Ids of the selected probe nodes, ascending.
    pub selected_nodes: Vec<usize>,
}

/// Solve the weighted structural vertex cut.
///
/// # Panics
/// If some source-to-sink path contains no probe node, which would mean the DAG
/// admits a match no cover can catch. The builder cannot produce such a path:
/// every lane ends at a terminal range, and a terminal range is a probe.
pub fn minimum_vertex_cut(graph: &PathGraph, weights: ProbeWeight) -> CutResult {
    let capacities: Vec<u64> = graph
        .nodes
        .iter()
        .map(|node| graph.node_capacity(node, weights))
        .collect();
    let finite_sum = graph
        .nodes
        .iter()
        .zip(&capacities)
        .filter(|(node, _)| node.probe.is_some())
        .try_fold(0u64, |acc, (_, &cap)| acc.checked_add(cap))
        .expect("sum of probe capacities overflowed u64");
    let infinite = finite_sum.saturating_add(1);

    let mut flow = Dinic::new(graph.nodes.len() * 2);
    let input = |node: usize| node * 2;
    let output = |node: usize| node * 2 + 1;
    for (node, &cap) in graph.nodes.iter().zip(&capacities) {
        flow.add_edge(
            input(node.id),
            output(node.id),
            if node.probe.is_some() { cap } else { infinite },
        );
    }
    for edge in &graph.edges {
        flow.add_edge(output(edge.from), input(edge.to), infinite);
    }

    let source = output(graph.source);
    let sink = input(graph.sink);
    let value = flow.max_flow(source, sink);
    assert!(
        value < infinite,
        "graph contains an uncuttable source/sink path"
    );
    let reachable = flow.reachable(source);
    let selected_nodes = graph
        .nodes
        .iter()
        .filter(|node| {
            node.probe.is_some() && reachable[input(node.id)] && !reachable[output(node.id)]
        })
        .map(|node| node.id)
        .collect();

    CutResult {
        value,
        selected_nodes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphEdge, GraphNode, NodeKind, PathGraph, Probe, ProbeSet};

    fn node(id: usize, name: &str, cap: Option<u64>) -> GraphNode {
        GraphNode {
            id,
            kind: NodeKind::Junction {
                label: name.to_string(),
                offset: None,
            },
            probe: cap.map(|tf| Probe {
                set: ProbeSet::Point { id: id as u16 },
                term_frequency: tf,
                row_frequency: tf,
                residual_term_frequency: tf,
                residual_row_frequency: tf,
                label: name.to_string(),
                detail: String::new(),
            }),
        }
    }

    /// The reason a global cut beats per-alignment local choices: two lanes that
    /// converge are blocked once at the join, not twice at their own terminals.
    #[test]
    fn shared_suffix_beats_two_local_choices() {
        // s -> a(4) -> x(6) -> t
        // s -> b(4) -> x(6) -> t
        let graph = PathGraph {
            needle: b"test".to_vec(),
            nodes: vec![
                node(0, "s", None),
                node(1, "a", Some(4)),
                node(2, "b", Some(4)),
                node(3, "x", Some(6)),
                node(4, "t", None),
            ],
            edges: vec![
                GraphEdge { from: 0, to: 1 },
                GraphEdge { from: 0, to: 2 },
                GraphEdge { from: 1, to: 3 },
                GraphEdge { from: 2, to: 3 },
                GraphEdge { from: 3, to: 4 },
            ],
            source: 0,
            sink: 4,
            contained: Vec::new(),
            stats: Default::default(),
            dictionary_size: 5,
            total_codes: 10,
            total_rows: 10,
        };
        let cut = minimum_vertex_cut(&graph, ProbeWeight::TermFrequency);
        assert_eq!(cut.value, 6);
        assert_eq!(cut.selected_nodes, vec![3]);
    }
}
