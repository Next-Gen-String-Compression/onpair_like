#pragma once

// Codes to bit mask (matcher/), bit mask to rows (resolver/), and the block
// driver joining them. See docs/prefilter.md section 6.

#include <algorithm>
#include <array>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <vector>

#include "../cover.hpp"
#include "resolver/resolver.hpp"

namespace fsst::search::prefilter::scan {

// One code stream and its rows: `offsets_len` ascending row offsets, the
// last one equal to `len`.
struct Stream {
    const uint8_t* codes;
    size_t len;
    const uint32_t* row_offsets;
    size_t offsets_len;

    size_t row_count() const { return offsets_len == 0 ? 0 : offsets_len - 1; }
    size_t row_start(size_t row) const { return row_offsets[row]; }
    size_t row_end(size_t row) const { return row_offsets[row + 1]; }
};

// The check stage two asks of a hit, passes(hit_index, row_start, row_end).
// This one passes every hit: the rows holding a covered code, a sound superset.
struct AcceptAll {
    bool passes(size_t, size_t, size_t) const { return true; }
};

constexpr size_t BLOCK = 512;
// The block the per-row scan reads a selected row in.
constexpr size_t ROW_BLOCK = 64;
// A block buffer carries 16 codes of padding: the pair compare reads one
// code past the block.
using Block = std::array<uint8_t, BLOCK + 16>;
// One mask word per 64 codes of a block.
template <size_t W>
using Bits = std::array<uint64_t, W>;
using Mask = Bits<BLOCK / 64>;
template <size_t W>
void clear_from(Bits<W>& bits, size_t valid) {
    if (valid >= 64 * W) return;
    bits[valid / 64] &= (uint64_t{1} << (valid % 64)) - 1;
    for (size_t w = valid / 64 + 1; w < bits.size(); ++w) bits[w] = 0;
}

// step(block, at, valid): 64 * W codes starting at stream index `at`, of
// which the first `valid` are the ones to scan. The code after a block is
// readable: the stream's next code, or the padding of the copied last block.
template <size_t W, typename Step>
void block_at(const uint8_t* codes, size_t len, size_t at, size_t valid, Step step) {
    if (at + 64 * W < len) return step(codes + at, at, valid);
    std::array<uint8_t, 64 * W + 16> tail{};
    std::memcpy(tail.data(), codes + at, len - at);
    step(tail.data(), at, valid);
}

template <size_t W, typename Step>
void blocks(const uint8_t* codes, size_t len, Step step) {
    for (size_t at = 0; at < len; at += 64 * W) block_at<W>(codes, len, at, std::min(64 * W, len - at), step);
}

// The stream's last code pairs with padding, so its bit is reset to the
// plain membership test. `word` holds the bit of block[valid - 1].
inline void last_pair_bit(const ProbeCover& cover, const uint8_t* block, size_t valid, uint64_t& word) {
    const uint64_t bit = uint64_t{1} << ((valid - 1) % 64);
    word = cover.contains(block[valid - 1]) ? word | bit : word & ~bit;
}

template <typename M, typename Check>
void both_stages(const ProbeCover& cover, const Stream& s, const Check& check, std::vector<size_t>& out) {
    M matcher(cover);
    resolver::GallopSeek resolver(s.row_offsets, s.offsets_len);
    const bool pairs = !cover.pairs.empty();
    Mask bits{};
    blocks<BLOCK / 64>(s.codes, s.len, [&](const uint8_t* block, size_t at, size_t valid) {
        if (!matcher.check(block, bits)) return;
        clear_from(bits, valid);
        if (pairs && valid > 0 && at + valid == s.len) last_pair_bit(cover, block, valid, bits[(valid - 1) / 64]);
        resolver.rows(bits, at, check, out);
    });
}

// The rows of `selection` read from `codes`, the stream from index `base`
// on, which holds a block and the code after it from each of their block
// starts. One block per pass and the test at its end, so a short row loops
// once. AT_END: the rows reach the stream's end, where the last code's pair
// compare reads padding and only the plain membership test counts.
template <bool AT_END, typename M, typename Check>
void rows_from(const M& matcher, const ProbeCover& cover, const uint8_t* codes, size_t base, const Stream& s,
               const uint32_t* selection, size_t count, const Check& check, std::vector<size_t>& out) {
    const bool pairs = !cover.pairs.empty();
    for (size_t k = 0; k < count; ++k) {
        const size_t row = selection[k];
        const size_t start = s.row_start(row), end = s.row_end(row);
        size_t at = start;
        do {
            const uint8_t* block = codes + (at - base);
            const size_t valid = std::min(ROW_BLOCK, end - at);
            const bool hit = matcher.row(block, valid, [&](size_t i) {
                if (AT_END && pairs && at + i + 1 == s.len && !cover.contains(block[i])) return false;
                return check.passes(at + i, start, end);
            });
            if (hit) {
                out.push_back(row);
                break;
            }
            at += ROW_BLOCK;
        } while (at < end);
    }
}

// The matcher over each row of `selection` from its first code, ROW_BLOCK
// codes at a time, running past the row end; the hits past it are dropped and
// every other hit is the row's, so no resolver is asked. The row goes out on
// its first hit that passes `check`, and its later blocks are never read.
// Rows whose blocks lie inside the stream read it in place; the few at its
// end read a padded copy.
template <typename M, typename Check>
void selected_rows(const ProbeCover& cover, const Stream& s, const uint32_t* selection, size_t count,
                   const Check& check, std::vector<size_t>& out) {
    M matcher(cover);
    const uint32_t* inside = std::partition_point(
        selection, selection + count, [&](uint32_t row) { return s.row_end(row) + ROW_BLOCK + 1 <= s.len; });
    const size_t n = static_cast<size_t>(inside - selection);
    rows_from<false>(matcher, cover, s.codes, 0, s, selection, n, check, out);
    if (n == count) return;
    const size_t base = s.row_start(*inside);
    std::vector<uint8_t> padded(s.codes + base, s.codes + s.len);
    padded.resize(padded.size() + ROW_BLOCK + 16, 0);
    rows_from<true>(matcher, cover, padded.data(), base, s, inside, count - n, check, out);
}

}  // namespace fsst::search::prefilter::scan
