#pragma once

// The planner's alignment graph and chosen cut, for logging and figures.
// Paper tooling, removable as a whole: this directory, the plan_log target in
// search/CMakeLists.txt, and the graphs, origin and cut that Planned carries
// beside its analysis.

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
inline Trace trace(const uint8_t* pattern, size_t n, const Dictionary& dict, const Frequency& freq,
                   size_t /*row_count, ignored as in analyze*/) {
    assert(n > 0);
    Planned p = plan(dict, pattern, n, freq);
    Trace t{std::move(p.graph), {}, {}, std::move(p.analysis)};
    t.cut.assign(t.graph.edges.size(), false);
    t.paired.assign(t.graph.edges.size(), {});
    for (uint32_t at : p.cut) {
        const Edge& e = p.cut_graph.edges[at];
        if (e.probe == Probe::Pair)
            t.paired[p.origin[at]].push_back(e.pair.first);
        else
            t.cut[p.origin[at]] = true;
    }
    return t;
}

// Ascending rows holding a covered code of a non-empty pattern's analysis.
inline void superset_rows(const scan::Stream& s, const Analysis& analysis, std::vector<size_t>& out) {
    execute(analysis, s, scan::AcceptAll{}, out);
}

}  // namespace fsst::search::prefilter
