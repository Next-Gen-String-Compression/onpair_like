#pragma once

// Pattern to probe cover: build the alignment graph, unroll it for pairs, cut
// it, price the cuts. A minimum cut is linear in its edge weights and the
// scan's cost is not: a kernel takes eight codes for the price of one, and a
// range costs the nibble table nothing. So two cuts stand in for it, one per
// end of that: by frequency alone, for covers wide enough that a code costs
// the kernel nothing, and by eq_or's fitted cost, for covers narrow enough
// that it pays for every code. scan_ns prices both as the covers they become
// and the cheaper is the plan. A cover between the two, where the batched
// kernel wins, can be missed, which is accepted.

#include <cmath>
#include <cstdint>
#include <vector>

#include "graph.hpp"
#include "mincut.hpp"
#include "scan/policy/policy.hpp"
#include "scan/walk/walk.hpp"

namespace fsst::search::prefilter {

struct Analysis {
    ProbeCover cover;  // empty when anchored: the row loop needs no cover
    uint32_t covered_frequency;
    uint32_t total_frequency;
    scan::Walk walk;
    bool matches_all;  // the empty pattern: every row, no scan
};

// A cut as the cover it becomes and what that costs.
struct Priced {
    ProbeCover cover;
    uint32_t covered;  // codes the cover matches in the indexed stream
    double scan_ns;
    std::vector<uint32_t> cut;  // indices into the cut graph's edges
};

// The cheaper by scan_ns of the two cuts over the indexed stream. The second
// weighs an edge in picoseconds: a verification per covered code, and each
// point, range and pair at eq_or's slope over every code of the stream.
inline Priced cheapest_cover(const CutGraph& graph, const Frequency& freq) {
    using scan::policy::Match;
    using scan::policy::Shape;
    auto ps = [](double ns) { return static_cast<uint64_t>(std::llround(ns * 1000.0)); };
    auto at = [](size_t points, size_t ranges, size_t pairs) {
        return scan::policy::ns_per_code(Match::EqOr, Shape{points, ranges, pairs});
    };
    const size_t code_count = freq.total;
    const double codes = static_cast<double>(code_count);
    const uint64_t point = ps(codes * (at(1, 0, 0) - at(0, 0, 0)));
    const uint64_t range = ps(codes * (at(0, 1, 0) - at(0, 0, 0)));
    const uint64_t pair = ps(codes * (at(0, 0, 1) - at(0, 0, 0)));
    const uint64_t verify = ps(scan::policy::VERIFY_NS_PER_HIT);

    auto price = [&](std::vector<uint32_t> cut) {
        std::vector<const Edge*> edges;
        for (uint32_t i : cut) edges.push_back(&graph.edges[i]);
        ProbeCover cover = from_edge_cut(edges);
        uint32_t covered = freq.of_cover(cover);
        double ns = scan::policy::scan_ns(cover, covered, code_count);
        return Priced{std::move(cover), covered, ns, std::move(cut)};
    };
    Priced selective = price(min_cut(graph, [](const Edge& e) { return uint64_t{e.frequency}; }));
    Priced balanced = price(min_cut(graph, [&](const Edge& e) {
        return verify * e.frequency + point * e.points + range * e.ranges + pair * e.pairs;
    }));
    return balanced.scan_ns < selective.scan_ns ? balanced : selective;
}

// What plan settles on: the analysis, and the graphs and cut behind it for
// tools/trace.hpp.
struct Planned {
    AlignmentGraph graph;
    CutGraph cut_graph;
    std::vector<uint32_t> origin;  // per cut graph edge, the alignment edge it came from
    std::vector<uint32_t> cut;     // the chosen cut, indices into cut_graph.edges
    Analysis analysis;
};

inline Planned plan(const Dictionary& dict, const uint8_t* pattern, size_t n, const Frequency& freq) {
    Planned p;
    p.graph = build_alignment_graph(dict, pattern, n, freq);
    p.cut_graph = CutGraph::with_pairs(p.graph, freq, &p.origin);
    Priced c = cheapest_cover(p.cut_graph, freq);
    p.cut = std::move(c.cut);
    p.analysis = Analysis{std::move(c.cover), c.covered, freq.total, scan::Walk(p.graph, pattern), false};
    return p;
}

}  // namespace fsst::search::prefilter
