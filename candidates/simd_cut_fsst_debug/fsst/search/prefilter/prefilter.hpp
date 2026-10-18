#pragma once

// The prefilter's entry points: analyze a pattern into a sound probe cover
// and its walk, then run the cover over a code stream, exact through the
// walk or as the superset of rows holding a covered code.

#include <cstdint>
#include <vector>

#include "plan.hpp"
#include "scan/execute.hpp"

namespace fsst::search::prefilter {

struct Analysis {
    ProbeCover cover;  // empty when anchored: the row loop needs no cover
    uint32_t covered_frequency;
    uint32_t total_frequency;
    scan::Walk walk;
    bool matches_all;  // the empty pattern: every row, no scan
};

inline Analysis analyze(const uint8_t* pattern, size_t n, const Dictionary& dict, const Frequency& freq,
                        size_t row_count, Anchor anchor = Anchor::None) {
    if (n == 0) return Analysis{ProbeCover{}, 0, freq.total, {}, true};
    if (anchor != Anchor::None) {
        // one layout, so nothing to cut and no cover to price: the walk alone decides, per row
        AlignmentGraph graph = build_alignment_graph(dict, pattern, n, freq, anchor);
        return Analysis{ProbeCover{}, 0, freq.total, scan::Walk(graph, pattern), false};
    }
    Planned p = plan(dict, pattern, n, freq, row_count);
    return Analysis{std::move(p.cover), p.covered, freq.total, std::move(p.walk), false};
}

// The analysis's planned scan over `s`, each hit decided by `check`. A
// non-empty pattern with an empty cover appends nothing; the empty pattern
// appends every row without a scan.
template <typename Check>
void execute(const Analysis& analysis, const scan::Stream& s, const Check& check, std::vector<size_t>& out) {
    if (analysis.matches_all) {
        for (size_t row = 0; row < s.row_count(); ++row) out.push_back(row);
        return;
    }
    scan::execute(analysis.cover, s, analysis.covered_frequency, analysis.total_frequency, check, out);
}

// Ascending rows containing the pattern: every hit goes through the walk.
// `dict` is the table the analysis was built over.
inline void candidate_rows(const scan::Stream& s, const Dictionary& dict, const Analysis& analysis,
                           std::vector<size_t>& out) {
    execute(analysis, s, scan::WalkCheck{analysis.walk, dict, s.codes}, out);
}

// Scans of one row group restricted to a selection of its rows, keeping their
// scratch across queries.
class Finder {
   public:
    // candidate_rows restricted to the `count` ascending row indices in
    // `selection`. A selected row costs the per-row scan its codes plus one
    // block of per-row overhead; the full scan costs every code once, and
    // the rows not selected are dropped afterwards.
    void find(const scan::Stream& s, const Dictionary& dict, const Analysis& analysis, const uint32_t* selection,
              size_t count, std::vector<size_t>& out) {
        if (analysis.matches_all) {
            out.insert(out.end(), selection, selection + count);
            return;
        }
        if (count == 0) return;
        const scan::WalkCheck check{analysis.walk, dict, s.codes};
        const size_t avg_codes = s.len / s.row_count();
        const size_t per_row_cost = count * (avg_codes + scan::ROW_BLOCK);
        if (per_row_cost < s.len) {
            scan::execute_selected(analysis.cover, s, analysis.covered_frequency, analysis.total_frequency, selection,
                                   count, check, out);
            return;
        }
        const size_t first = out.size();
        scan::execute(analysis.cover, s, analysis.covered_frequency, analysis.total_frequency, check, out);
        keep_selected(s.row_count(), selection, count, out, first);
    }

   private:
    // Drops from out[first..] the rows `selection` does not hold, in place and
    // without branches: every row is written back, the write position
    // advancing only past a selected one.
    void keep_selected(size_t num_rows, const uint32_t* selection, size_t count, std::vector<size_t>& out,
                       size_t first) {
        selected_.assign(num_rows, 0);
        uint8_t* selected = selected_.data();
        for (size_t k = 0; k < count; ++k) selected[selection[k]] = 1;
        size_t* rows = out.data();
        size_t kept = first;
        for (size_t i = first; i < out.size(); ++i) {
            const size_t row = rows[i];
            rows[kept] = row;
            kept += selected[row];
        }
        out.resize(kept);
    }

    // One byte per row of the row group, 1 where the selection holds the row.
    std::vector<uint8_t> selected_;
};

}  // namespace fsst::search::prefilter
