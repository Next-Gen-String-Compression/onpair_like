#pragma once

// How often each code stands in the stream, with the cumulative sum so a code
// range is priced in one subtraction. Literal bytes behind escapes count as the
// code they look like, which is what the matcher sees. The counts come from a
// pass over the stream, or from the symbol table of an encoder that called
// fsst_set_frequency(). Advisory weights only; soundness never reads them.

#include <array>
#include <cstddef>
#include <cstdint>
#include <vector>

#include "cover.hpp"

namespace fsst::search::prefilter {

struct Frequency {
    std::array<uint32_t, 256> count{};
    std::array<uint32_t, 257> cum{};  // cum[c] = sum of count[0..c)
    uint32_t total = 0;

    static Frequency of(const uint8_t* codes, size_t len) {
        Frequency f;
        for (size_t i = 0; i < len; ++i) ++f.count[codes[i]];
        f.finish();
        return f;
    }

    // The counts an encoder attached to its symbol table, fsst_decoder_t::count.
    static Frequency of_counts(const uint32_t* counts) {
        Frequency f;
        for (size_t c = 0; c < 256; ++c) f.count[c] = counts[c];
        f.finish();
        return f;
    }

    void finish() {
        uint64_t sum = 0;
        for (size_t c = 0; c < 256; ++c) {
            cum[c + 1] = cum[c] + count[c];
            sum += count[c];
        }
        total = static_cast<uint32_t>(sum);
    }

    uint32_t of_range(CodeRange r) const { return cum[static_cast<size_t>(r.last) + 1] - cum[r.begin]; }

    // `codes` distinct, so the sum stays within total.
    template <typename Codes>
    uint32_t of_codes(const Codes& codes) const {
        uint32_t sum = 0;
        for (uint8_t c : codes) sum += count[c];
        return sum;
    }

    // Positions where the pair stands, under independence: counts alone do not say
    // how often two codes follow one another, so a rare sequence of two common codes
    // prices as though it were common.
    uint32_t pair_estimate(CodePair p) const {
        if (total == 0) return 0;
        const uint64_t joint = static_cast<uint64_t>(count[p.first]) * count[p.second] / total;
        return static_cast<uint32_t>(joint);
    }

    uint32_t of_cover(const ProbeCover& cover) const {
        uint32_t sum = of_codes(cover.points);
        for (const CodeRange& r : cover.ranges) sum += of_range(r);
        for (CodePair p : cover.pairs) sum += pair_estimate(p);
        return sum;
    }
};

}  // namespace fsst::search::prefilter
