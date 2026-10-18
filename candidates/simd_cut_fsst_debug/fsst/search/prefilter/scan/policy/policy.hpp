#pragma once

// Which matcher and which resolver, from the cover and the region, and what
// the scan they make costs. Every constant is a fit to the sweeps in bench/;
// refit with `prefilter_matcher_sweep --refit` and `prefilter_resolver_sweep
// --refit` and paste the printed blocks here.

#include <algorithm>
#include <cmath>
#include <cstddef>
#include <cstdint>

#include "../../cover.hpp"
#include "../matcher/limits.hpp"
#include "../scan.hpp"

namespace fsst::search::prefilter::scan::policy {

// What the planner reads about the region. No code values are inspected.
struct Facts {
    size_t expected_hits;
    size_t code_count;
    size_t row_count;
};

struct Shape {
    size_t tokens;
    size_t ranges;
    size_t pairs = 0;

    static Shape of(const ProbeCover& cover) { return {cover.points.size(), cover.ranges.size(), cover.pairs.size()}; }
};

enum class Match { EqOr, Range, NibbleN8K, Nibble };
enum class Resolve { LinearSeek, GallopSeek };

struct Plan {
    Match matcher;
    Resolve resolver;
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

// ns per code; k tokens, r ranges, p pairs. Fitted on aarch64 Apple M4 Pro
// neon, novel_mask_2026-09-15_16-09-02.csv. The x86 kernel sets are not
// fitted: these rows stand in until prefilter_matcher_sweep --refit runs there.
inline double ns_per_code(Match m, Shape s) {
    const double k = static_cast<double>(s.tokens);
    const double r = static_cast<double>(s.ranges);
    const double p = static_cast<double>(s.pairs);
    switch (m) {
        case Match::EqOr: return 0.00330 + 0.00721 * k + 0.01131 * r + 0.01674 * p;
        case Match::NibbleN8K: return 0.02449 + 0.01230 * r;
        case Match::Nibble: return 0.03860;
        case Match::Range: return 0.00350 + 0.01078 * r;
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

// Codes one pass of the movemask covers, the unit the skip flag decides over.
constexpr double PACK_GROUP = 128.0;
// The lane OR the flag pays on every group and the movemask it saves on a
// group with no match, both ns per group, from the eq_or K = 1 ends of the
// same sweep: +0.026 ns per code where every group matches, -0.0006 where none does.
constexpr double SKIP_REDUCTION_NS = 3.3;
constexpr double SKIP_PACK_NS = 3.4;

// What the skip flag adds per code at this bit density; negative is worth taking.
inline double skip_ns_per_code(double density) {
    double no_match = std::exp(-PACK_GROUP * density);
    return (SKIP_REDUCTION_NS - SKIP_PACK_NS * no_match) / PACK_GROUP;
}

inline double density(Facts f) {
    return f.code_count == 0 ? 0.0 : static_cast<double>(f.expected_hits) / static_cast<double>(f.code_count);
}

inline double stage_one_ns_per_code(Shape s, double density) {
    return ns_per_code(select_matcher(s), s) + std::min(skip_ns_per_code(density), 0.0);
}

// Stage two, fitted on aarch64 Apple M4 Pro neon, from novel_resolve_2026-09-15_14-15-29.csv:
// one mask word read in a block that hit, one row LinearSeek emits, one row
// it walks past, one halving of a GallopSeek search.
constexpr double WORD_NS = 0.14;
constexpr double LINEAR_SEEK_ROW_NS = 1.78;
constexpr double LINEAR_SEEK_CROSS_NS = 0.39;
constexpr double GALLOP_SEEK_STEP_NS = 2.55;
// One hit through the alignment walk. onpair's measurement; refit once the walk exists.
constexpr double WALK_NS_PER_HIT = 8.0;

inline double seek_ns_per_row(Resolve r, double g) {
    return r == Resolve::LinearSeek ? LINEAR_SEEK_ROW_NS + LINEAR_SEEK_CROSS_NS * g
                                    : GALLOP_SEEK_STEP_NS * std::log2(1.0 + g);
}

inline double stage_two_ns(Resolve r, double words, double emitted, double crossed) {
    if (emitted <= 0.0) return WORD_NS * words;
    return WORD_NS * words + emitted * seek_ns_per_row(r, crossed / emitted);
}

// Rows the mask is expected to name: a row of x expected hits holds one with
// probability 1 - exp(-x).
inline double hit_rows(double expected_hits, double rows) { return rows * (1.0 - std::exp(-expected_hits / rows)); }

inline Resolve select_resolver(Facts f) {
    double rows = static_cast<double>(f.row_count);
    double g = rows / std::max(hit_rows(static_cast<double>(f.expected_hits), rows), 1.0);
    return seek_ns_per_row(Resolve::GallopSeek, g) < seek_ns_per_row(Resolve::LinearSeek, g) ? Resolve::GallopSeek
                                                                                              : Resolve::LinearSeek;
}

inline Plan select(const ProbeCover& cover, Facts f) {
    return {select_matcher(Shape::of(cover)), select_resolver(f), skip_ns_per_code(density(f)) < 0.0};
}

struct Region {
    size_t code_count;
    size_t row_count;
};

// Expected ns to scan `cover` over `region` and walk every hit, given the
// `covered` codes it matches there. An empty cover proves no row matches.
inline double scan_ns(const ProbeCover& cover, uint32_t covered, Region region) {
    if (cover.empty() || region.code_count == 0 || region.row_count == 0) return 0.0;
    double codes = static_cast<double>(region.code_count);
    double rows = static_cast<double>(region.row_count);
    double hits = static_cast<double>(covered);
    double stage_one = codes * stage_one_ns_per_code(Shape::of(cover), hits / codes);
    double emitted = hit_rows(hits, rows);
    double blocks_hit = 1.0 - std::exp(-hits / codes * static_cast<double>(BLOCK));
    double words = codes / 64.0 * blocks_hit;
    double stage_two = std::min(stage_two_ns(Resolve::LinearSeek, words, emitted, rows),
                                stage_two_ns(Resolve::GallopSeek, words, emitted, rows));
    return stage_one + stage_two + hits * WALK_NS_PER_HIT;
}

// The lambda at which a cut's weight, frequency + lambda * comparisons, is
// its scan cost up to a constant: eq_or's ns per code and point, times the
// codes scanned, over the walk's ns per hit. The slope is read off the fit
// so the per-ISA rows above stay its only copy.
inline uint64_t cost_lambda(Region region) {
    const double per_point = ns_per_code(Match::EqOr, Shape{2, 0}) - ns_per_code(Match::EqOr, Shape{1, 0});
    return static_cast<uint64_t>(per_point * static_cast<double>(region.code_count) / WALK_NS_PER_HIT);
}

}  // namespace fsst::search::prefilter::scan::policy
