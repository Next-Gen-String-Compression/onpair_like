#pragma once

// Which matcher, from the cover and the region, and what the scan it makes
// costs. The kernel rows are a fit to the sweep in bench/; refit with
// `prefilter_matcher_sweep --refit` and paste the printed block here.

#include <cmath>
#include <cstddef>
#include <cstdint>

#include "../../cover.hpp"
#include "../matcher/limits.hpp"

namespace fsst::search::prefilter::scan::policy {

// What the planner reads about the region. No code values are inspected.
struct Facts {
    size_t expected_hits;
    size_t code_count;
};

struct Shape {
    size_t tokens;
    size_t ranges;
    size_t pairs = 0;

    static Shape of(const ProbeCover& cover) { return {cover.points.size(), cover.ranges.size(), cover.pairs.size()}; }
};

enum class Match { EqOr, Range, NibbleN8K, Nibble };

struct Plan {
    Match matcher;
    bool skip;
};

constexpr Match KERNELS[] = {Match::EqOr, Match::Range, Match::NibbleN8K, Match::Nibble};

inline size_t batches(Shape s) { return (s.tokens + matcher::PER_BATCH - 1) / matcher::PER_BATCH; }

// Only eq_or compares pairs.
inline bool takes(Match m, Shape s) {
    switch (m) {
        case Match::EqOr: return s.tokens > 0 || s.pairs > 0;
        case Match::Range: return s.tokens == 0 && s.pairs == 0 && s.ranges > 0;
        case Match::NibbleN8K: return s.pairs == 0 && s.tokens > 0 && batches(s) <= matcher::MAX_BATCHES;
        case Match::Nibble: return s.pairs == 0;
    }
    return false;
}

// ns per code; k tokens, r ranges, p pairs. AVX-512 fitted on x86_64 Xeon
// Platinum 8375C avx512bw, novel_mask_2026-10-18_05-37-11.csv; everything else
// on aarch64 Apple M4 Pro neon, novel_mask_2026-09-15_16-09-02.csv. AVX2 is not
// fitted and takes the NEON rows.
inline double ns_per_code(Match m, Shape s) {
    const double k = static_cast<double>(s.tokens);
    const double r = static_cast<double>(s.ranges);
    const double p = static_cast<double>(s.pairs);
    switch (m) {
#if defined(__AVX512BW__)
        case Match::EqOr: return 0.00741 + 0.00911 * k + 0.00142 * r + 0.01480 * p;
        case Match::NibbleN8K: return 0.02597 + 0.00881 * r;
        case Match::Nibble: return 0.02892;
        case Match::Range: return 0.01425 + 0.00761 * r;
#else
        case Match::EqOr: return 0.00330 + 0.00721 * k + 0.01131 * r + 0.01674 * p;
        case Match::NibbleN8K: return 0.02449 + 0.01230 * r;
        case Match::Nibble: return 0.03860;
        case Match::Range: return 0.00350 + 0.01078 * r;
#endif
    }
    return INFINITY;
}

inline Match select_matcher(Shape s) {
    Match best = Match::Nibble;
    double cost = INFINITY;
    for (Match m : KERNELS) {
        if (!takes(m, s)) continue;
        double ns = ns_per_code(m, s);
        if (ns < cost) {
            cost = ns;
            best = m;
        }
    }
    return best;
}

// Hit density below which skipping the movemask on a group with no match
// pays. The flag adds a lane OR on every group of 128 codes, 3.3 ns, and saves
// the movemask, 3.4 ns, on each group with no match, an exp(-128 * density)
// share if the hits fall evenly: break-even at ln(3.4 / 3.3) / 128. Both are
// from the eq_or K = 1 ends of the sweep. Hits cluster, so this is an order
// and not a point.
#if defined(__AVX512BW__)
// On AVX-512 the compare has already written the mask word, so there is no
// movemask to skip and the flag can only cost: onpair's sweep measured it
// positive at every selectivity.
constexpr double SKIP_BELOW_DENSITY = 0.0;
#else
constexpr double SKIP_BELOW_DENSITY = 0.00023;
#endif

inline double density(Facts f) {
    return f.code_count == 0 ? 0.0 : static_cast<double>(f.expected_hits) / static_cast<double>(f.code_count);
}

inline Plan select(const ProbeCover& cover, Facts f) {
    return {select_matcher(Shape::of(cover)), density(f) < SKIP_BELOW_DENSITY};
}

// One hit through the alignment walk. onpair's measurement; refit once the walk exists.
constexpr double VERIFY_NS_PER_HIT = 8.0;

// Expected ns to scan `cover` over `code_count` codes and walk every hit,
// given the `covered` codes it matches there: stage one at the kernel the
// shape gets and the walk per hit. Stage two, the resolve, is under 3% of
// either and left out. An empty cover proves no row matches.
inline double scan_ns(const ProbeCover& cover, uint32_t covered, size_t code_count) {
    if (cover.empty() || code_count == 0) return 0.0;
    const Shape shape = Shape::of(cover);
    const double stage_one = static_cast<double>(code_count) * ns_per_code(select_matcher(shape), shape);
    return stage_one + static_cast<double>(covered) * VERIFY_NS_PER_HIT;
}

}  // namespace fsst::search::prefilter::scan::policy
