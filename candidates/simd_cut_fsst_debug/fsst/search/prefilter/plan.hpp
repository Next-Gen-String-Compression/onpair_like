#pragma once

// Pattern to probe cover: build the alignment graph, unroll it for pairs,
// cut it under frequency + lambda * (points + 2 * ranges + 2 * pairs) at the
// lambda the cost model names and at the narrowest, price both with scan_ns
// and keep the cheaper.

#include <algorithm>
#include <cstdint>
#include <vector>

#include "graph.hpp"
#include "mincut.hpp"
#include "scan/policy/policy.hpp"
#include "scan/walk/walk.hpp"

namespace fsst::search::prefilter {

struct Planned {
    ProbeCover cover;
    uint32_t covered;  // codes the cover matches in the indexed stream
    double scan_ns;
    scan::Walk walk;
};

inline uint64_t weight_at(const Edge& e, uint64_t lambda) {
    return e.frequency + lambda * (e.points + 2 * e.ranges + 2 * e.pairs);
}

// Two cuts, priced by scan_ns: the one at the lambda where a cut's weight is
// its scan cost under the linear part of the model, and the narrowest, where
// the kernel choice bends that model. `chosen`, when given, receives the
// winning cut as indices into graph.edges (search/tools/trace.hpp).
inline Planned cheapest_cover(const CutGraph& graph, const Frequency& freq, scan::policy::Region region,
                              std::vector<uint32_t>* chosen = nullptr) {
    const uint64_t ceiling = freq.total;
    MinCut solver(graph);
    Planned best{};
    bool priced = false;
    std::vector<const Edge*> edges;  // one buffer for every cut priced
    auto price = [&](const std::vector<uint32_t>& cut) {
        edges.clear();
        for (uint32_t at : cut) edges.push_back(&graph.edges[at]);
        ProbeCover cover = from_edge_cut(edges);
        uint32_t covered = freq.of_cover(cover);
        double ns = scan::policy::scan_ns(cover, covered, region);
        if (!priced || ns < best.scan_ns) {
            best = Planned{std::move(cover), covered, ns, {}};
            if (chosen) *chosen = cut;
            priced = true;
        }
    };
    std::vector<uint32_t> narrowest = solver.solve(graph, [&](const Edge& e) { return weight_at(e, ceiling + 1); });
    price(narrowest);
    const uint64_t lambda = std::min(scan::policy::cost_lambda(region), ceiling);
    const std::vector<uint32_t>& cut = solver.solve(graph, [&](const Edge& e) { return weight_at(e, lambda); });
    if (cut != narrowest) price(cut);
    return best;
}

inline Planned plan(const Dictionary& dict, const uint8_t* pattern, size_t n, const Frequency& freq,
                    size_t row_count) {
    AlignmentGraph graph = build_alignment_graph(dict, pattern, n, freq);
    Planned p = cheapest_cover(CutGraph::with_pairs(graph, freq), freq, scan::policy::Region{freq.total, row_count});
    p.walk = scan::Walk(graph, pattern);
    return p;
}

}  // namespace fsst::search::prefilter
