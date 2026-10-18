#pragma once

// The planner's alignment graph and chosen cut, for logging and figures.
// Paper tooling, removable as a whole: this directory, the plan_log target in
// search/CMakeLists.txt, and the two optional outputs it reads, the `origin`
// of CutGraph::with_pairs and the `chosen` cut of cheapest_cover.

#include <cassert>
#include <cstdint>
#include <vector>

#include "../prefilter/prefilter.hpp"

namespace fsst::search::prefilter {

// The alignment graph, which edges the cut selects outright, and for each
// edge the codes before it the cut pairs it with instead.
struct Trace {
    AlignmentGraph graph;
    std::vector<bool> cut;
    std::vector<std::vector<uint8_t>> paired;
    Analysis analysis;  // what analyze returns for the same pattern
};

// Plans a non-empty pattern as analyze does, keeping the graph and the cut.
inline Trace trace(const uint8_t* pattern, size_t n, const Dictionary& dict, const Frequency& freq, size_t row_count) {
    assert(n > 0);
    Trace t{build_alignment_graph(dict, pattern, n, freq), {}, {}, {}};
    std::vector<uint32_t> origin, chosen;
    const CutGraph cg = CutGraph::with_pairs(t.graph, freq, &origin);
    Planned p = cheapest_cover(cg, freq, scan::policy::Region{freq.total, row_count}, &chosen);
    t.cut.assign(t.graph.edges.size(), false);
    t.paired.assign(t.graph.edges.size(), {});
    for (uint32_t at : chosen) {
        const Edge& e = cg.edges[at];
        if (e.probe == Probe::Pair)
            t.paired[origin[at]].push_back(e.pair.first);
        else
            t.cut[origin[at]] = true;
    }
    t.analysis = Analysis{std::move(p.cover), p.covered, freq.total, scan::Walk(t.graph, pattern), false};
    return t;
}

// Ascending rows holding a covered code of a non-empty pattern's analysis.
inline void superset_rows(const scan::Stream& s, const Analysis& analysis, std::vector<size_t>& out) {
    execute(analysis, s, scan::AcceptAll{}, out);
}

}  // namespace fsst::search::prefilter
