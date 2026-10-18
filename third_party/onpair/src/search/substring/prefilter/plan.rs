// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! Pattern to probe cover: build the alignment DAG, cut it, price the cuts.
//!
//! A minimum cut is linear in its edge weights and the scan's cost is not: a
//! kernel takes eight tokens for the price of one, and a range costs the
//! bitmap nothing. So two cuts stand in for it, one per end of that: by
//! frequency alone, for covers wide enough that a token costs the kernel
//! nothing, and by the compare kernel's fitted cost, for covers narrow enough
//! that it pays for every token. [`scan_ns`] prices both as the covers they
//! become and the cheaper is the plan. A cover between the two, where the
//! batched kernel wins, can be missed, which is accepted.
//!
//! Frequencies weight the cut but never alter its selected membership:
//! safety-valid stored weights may contain false zeroes, so pruning by
//! frequency would be unsound. Profitability remains a caller decision.

use super::PrefilterAnalysis;
use super::cover::ProbeCover;
use super::graph::{AlignmentGraph, Anchor, Edge, build_alignment_graph};
use super::mincut::min_cut;
use super::scan::Walk;
use super::scan::{VERIFY_NS_PER_HIT, scan_ns};
use crate::core::dictionary::CompactDictionaryView;
use crate::search::index::TokenFrequencyIndexView;

/// Compile a sound probe cover for `pattern` over `dict`, cheapest by the
/// scan cost model over the indexed stream: the alignment graph, the cut
/// chosen on it as edge ids, and the analysis that cut becomes.
pub(super) fn plan(
    dict: CompactDictionaryView<'_>,
    pattern: &[u8],
    frequencies: TokenFrequencyIndexView<'_>,
) -> (AlignmentGraph, Vec<u32>, PrefilterAnalysis) {
    let graph = build_alignment_graph(dict, pattern, Some(frequencies), Anchor::Anywhere);
    let (cover, covered, scan_ns, cut) = cheapest_cover(&graph, frequencies);
    let analysis = PrefilterAnalysis {
        probe_cover: cover,
        covered_frequency: covered,
        total_frequency: frequencies.total_frequency(),
        scan_ns,
        walk: Walk::from_graph(&graph, pattern),
        matches_all: false,
    };
    (graph, cut, analysis)
}

/// The cheaper by [`scan_ns`] of the two cuts over the indexed stream, with
/// what it covers, what it costs and the ids of its edges. The second weighs
/// an edge in picoseconds: a verification per covered code, and each token
/// and range at the compare kernel's slope over every code of the stream.
pub(super) fn cheapest_cover(
    graph: &AlignmentGraph,
    frequencies: TokenFrequencyIndexView<'_>,
) -> (ProbeCover, u32, f64, Vec<u32>) {
    let ps = |ns: f64| (ns * 1000.0).round() as u64;
    let code_count = frequencies.total_frequency() as usize;
    let codes = code_count as f64;
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    let (token, range) = {
        use super::scan::{Match, Shape, ns_per_code};
        let at = |tokens, ranges| ns_per_code(Match::EqOr, Shape { tokens, ranges });
        (
            ps(codes * (at(1, 0) - at(0, 0))),
            ps(codes * (at(0, 1) - at(0, 0))),
        )
    };
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    let (token, range) = (0, 0);
    let verify = ps(VERIFY_NS_PER_HIT);

    let price = |cut: &[u32]| {
        let edges: Vec<&Edge> = cut.iter().map(|&at| &graph.edges[at as usize]).collect();
        let cover = ProbeCover::from_edge_cut(&edges);
        let covered = cover_frequency(&cover, frequencies);
        let ns = scan_ns(&cover, covered, code_count);
        (cover, covered, ns, cut.to_vec())
    };
    let selective = price(&min_cut(&graph.edges, graph.nodes, |e| {
        u64::from(e.frequency())
    }));
    let balanced = price(&min_cut(&graph.edges, graph.nodes, |e| {
        let (points, ranges) = e.shape();
        verify * u64::from(e.frequency()) + token * u64::from(points) + range * u64::from(ranges)
    }));
    if balanced.2 < selective.2 {
        balanced
    } else {
        selective
    }
}

/// Codes `cover` matches in the indexed stream. Points and ranges are
/// disjoint, so each covered code is counted once.
pub(super) fn cover_frequency(cover: &ProbeCover, frequencies: TokenFrequencyIndexView<'_>) -> u32 {
    let points: u32 = cover
        .points()
        .iter()
        .map(|&t| frequencies.frequency(t))
        .sum();
    let ranges: u32 = cover
        .ranges()
        .iter()
        .map(|&r| frequencies.range_frequency(r))
        .sum();
    points + ranges
}
