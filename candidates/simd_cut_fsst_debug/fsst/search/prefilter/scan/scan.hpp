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

template <typename M, typename R, typename Check>
void both_stages(const ProbeCover& cover, const Stream& s, const Check& check, std::vector<size_t>& out) {
    M matcher(cover);
    R resolver(s.row_offsets, s.offsets_len);
    const bool pairs = !cover.pairs.empty();
    Mask bits{};
    blocks<BLOCK / 64>(s.codes, s.len, [&](const uint8_t* block, size_t at, size_t valid) {
        if (!matcher.check(block, bits)) return;
        clear_from(bits, valid);
        if (pairs && valid > 0 && at + valid == s.len) last_pair_bit(cover, block, valid, bits[(valid - 1) / 64]);
        resolver.rows(bits, at, check, out);
    });
}

// The matcher over each row of `selection` from its first code, in ROW_BLOCK
// codes at a time, running past the row end; the bits past it are cleared and every
// other hit is the row's, so no resolver is asked. The row goes out on its
// first hit that passes `check`, and its later blocks are never read.
template <typename M, typename Check>
void selected_rows(const ProbeCover& cover, const Stream& s, const uint32_t* selection, size_t count,
                   const Check& check, std::vector<size_t>& out) {
    M matcher(cover);
    const bool pairs = !cover.pairs.empty();
    Bits<1> bits{};
    std::array<uint8_t, ROW_BLOCK + 16> tail{};
    auto matches = [&](size_t start, size_t end) {
        for (size_t at = start; at < end; at += ROW_BLOCK) {
            // As block_at: the code after the block must be readable for the pair compare.
            const uint8_t* block = s.codes + at;
            if (at + ROW_BLOCK >= s.len) {
                tail.fill(0);
                std::memcpy(tail.data(), s.codes + at, s.len - at);
                block = tail.data();
            }
            if (!matcher.check(block, bits)) continue;
            const size_t valid = std::min(ROW_BLOCK, end - at);
            uint64_t set = valid < ROW_BLOCK ? bits[0] & ((uint64_t{1} << valid) - 1) : bits[0];
            if (pairs && at + valid == s.len) last_pair_bit(cover, block, valid, set);
            for (; set != 0; set &= set - 1)
                if (check.passes(at + static_cast<size_t>(__builtin_ctzll(set)), start, end)) return true;
        }
        return false;
    };
    for (size_t k = 0; k < count; ++k) {
        const size_t row = selection[k];
        if (matches(s.row_start(row), s.row_end(row))) out.push_back(row);
    }
}

}  // namespace fsst::search::prefilter::scan
