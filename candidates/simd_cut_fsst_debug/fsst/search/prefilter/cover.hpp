#pragma once

#include <algorithm>
#include <cstdint>
#include <vector>

namespace fsst::search::prefilter {

// Inclusive run of codes.
struct CodeRange {
    uint8_t begin;
    uint8_t last;

    static CodeRange none() { return CodeRange{1, 0}; }

    bool empty() const { return begin > last; }
    bool contains(uint8_t code) const { return begin <= code && code <= last; }
};

// Two adjacent codes: `first` at some position, `second` right after it.
struct CodePair {
    uint8_t first;
    uint8_t second;

    bool operator<(CodePair o) const { return first != o.first ? first < o.first : second < o.second; }
    bool operator==(CodePair o) const { return first == o.first && second == o.second; }
};

// The codes the scan probes for: single codes and inclusive runs, disjoint,
// plus code pairs. A pair's bit lands on its first code.
struct ProbeCover {
    std::vector<uint8_t> points;
    std::vector<CodeRange> ranges;
    std::vector<CodePair> pairs;

    bool empty() const { return points.empty() && ranges.empty() && pairs.empty(); }

    bool contains(uint8_t code) const {
        for (uint8_t p : points)
            if (p == code) return true;
        for (const CodeRange& r : ranges)
            if (r.contains(code)) return true;
        return false;
    }

    // Merge runs that overlap or abut, then file single-code runs as points
    // and the rest as ranges. Input in any order.
    static ProbeCover from_runs(std::vector<CodeRange> runs) {
        std::sort(runs.begin(), runs.end(), [](CodeRange a, CodeRange b) { return a.begin < b.begin; });
        std::vector<CodeRange> merged;
        for (CodeRange run : runs) {
            if (!merged.empty() && run.begin <= static_cast<size_t>(merged.back().last) + 1)
                merged.back().last = std::max(merged.back().last, run.last);
            else
                merged.push_back(run);
        }
        ProbeCover cover;
        for (CodeRange run : merged) {
            if (run.begin == run.last)
                cover.points.push_back(run.begin);
            else
                cover.ranges.push_back(run);
        }
        return cover;
    }
};

}  // namespace fsst::search::prefilter
