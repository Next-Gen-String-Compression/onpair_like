#pragma once

// The scan as the planner enters it: the region's facts, the plan, the run.
// The rest of what onpair keeps in scan/mod.rs beside the seam in scan.hpp.

#include <optional>
#include <vector>

#include "dispatch.hpp"
#include "policy/policy.hpp"

namespace fsst::search::prefilter::scan {

// Covered codes expected in this region: exact for the indexed population,
// a projection of it for a subset. Planning only, never correctness.
inline size_t expected_hits(size_t covered_frequency, size_t total_frequency, size_t code_count) {
    if (total_frequency == code_count) return covered_frequency;
    if (total_frequency == 0) return 0;
    unsigned __int128 projected = static_cast<unsigned __int128>(covered_frequency) * code_count / total_frequency;
    return projected > static_cast<unsigned __int128>(SIZE_MAX) ? SIZE_MAX : static_cast<size_t>(projected);
}

// None where the region admits nothing whatever runs over it: an empty
// cover proves no row matches, and a region with no rows has nothing to emit.
inline std::optional<policy::Facts> facts(const ProbeCover& cover, const Stream& s, size_t covered_frequency,
                                          size_t total_frequency) {
    if (cover.empty() || s.row_count() == 0) return std::nullopt;
    return policy::Facts{expected_hits(covered_frequency, total_frequency, s.len), s.len};
}

// The planned scan over one region under whatever stage two asks of a hit.
// `covered_frequency` and `total_frequency` are the frequency index's counts.
template <typename Check>
void execute(const ProbeCover& cover, const Stream& s, size_t covered_frequency, size_t total_frequency,
             const Check& check, std::vector<size_t>& out) {
    auto f = facts(cover, s, covered_frequency, total_frequency);
    if (!f) return;
    dispatch::run(policy::select(cover, *f), cover, s, check, out);
}

// The planned scan over the rows of `selection` only, each on its own. The
// facts are the whole region's, as the full scan's would be.
template <typename Check>
void execute_selected(const ProbeCover& cover, const Stream& s, size_t covered_frequency, size_t total_frequency,
                      const uint32_t* selection, size_t count, const Check& check, std::vector<size_t>& out) {
    auto f = facts(cover, s, covered_frequency, total_frequency);
    if (!f) return;
    dispatch::run_selected(policy::select(cover, *f), cover, s, selection, count, check, out);
}

}  // namespace fsst::search::prefilter::scan
