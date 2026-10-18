#pragma once

// Minimal harness for the scan tests: a failure counter, a generator, and the
// scalar oracles the matcher, resolver and driver tests compare against.

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <vector>

#include "scan.hpp"

inline int g_failures = 0;

#define CHECK(cond)                                                                   \
    do {                                                                              \
        if (!(cond)) {                                                                \
            std::fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #cond); \
            ++g_failures;                                                             \
        }                                                                             \
    } while (0)

#define CHECK_MSG(cond, ...)                                                          \
    do {                                                                              \
        if (!(cond)) {                                                                \
            std::fprintf(stderr, "%s:%d: CHECK failed: %s: ", __FILE__, __LINE__, #cond); \
            std::fprintf(stderr, __VA_ARGS__);                                        \
            std::fprintf(stderr, "\n");                                               \
            ++g_failures;                                                             \
        }                                                                             \
    } while (0)

struct Rng {
    uint64_t s;
    uint64_t next() {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        return s * 0x2545F4914F6CDD1DULL;
    }
    uint64_t below(uint64_t n) { return next() % n; }
};

inline int finish(const char* suite) {
    if (g_failures == 0)
        std::printf("%s: ok\n", suite);
    else
        std::printf("%s: %d failures\n", suite, g_failures);
    return g_failures == 0 ? 0 : 1;
}

namespace fsst::search::prefilter::scan {

// The scalar definition of the mask bit at i over codes[..end): a covered
// code, or the first of a covered pair whose second lies before `end`.
inline bool cover_matches(const ProbeCover& cover, const uint8_t* codes, size_t i, size_t end) {
    if (cover.contains(codes[i])) return true;
    if (i + 1 >= end) return false;
    for (CodePair p : cover.pairs)
        if (codes[i] == p.first && codes[i + 1] == p.second) return true;
    return false;
}

// The rows holding a matching position, over the whole stream.
inline std::vector<size_t> rows_oracle(const ProbeCover& cover, const Stream& s) {
    std::vector<size_t> out;
    for (size_t row = 0; row < s.row_count(); ++row)
        for (size_t i = s.row_start(row); i < s.row_end(row); ++i)
            if (cover_matches(cover, s.codes, i, s.len)) {
                out.push_back(row);
                break;
            }
    return out;
}

// The rows a mask over the whole stream names, each at its first bit that
// passes `check`.
template <typename Check>
std::vector<size_t> rows_of_mask(const std::vector<uint64_t>& mask, const std::vector<uint32_t>& offsets,
                                 const Check& check) {
    std::vector<size_t> rows;
    for (size_t word = 0; word < mask.size(); ++word) {
        for (uint64_t set = mask[word]; set; set &= set - 1) {
            const size_t code = word * 64 + static_cast<size_t>(__builtin_ctzll(set));
            const size_t row =
                static_cast<size_t>(std::upper_bound(offsets.begin(), offsets.end(), code) - offsets.begin()) - 1;
            if (!check.passes(code, offsets[row], offsets[row + 1])) continue;
            if (rows.empty() || rows.back() != row) rows.push_back(row);
        }
    }
    return rows;
}

}  // namespace fsst::search::prefilter::scan
