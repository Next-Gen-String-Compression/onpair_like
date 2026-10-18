#pragma once

// Stage two: bit mask in, candidate rows out.

#include <algorithm>
#include <array>
#include <cstddef>
#include <cstdint>
#include <vector>

namespace fsst::search::prefilter::scan::resolver {

constexpr size_t NONE = static_cast<size_t>(-1);

// The set bit at or after `from`, NONE if the block holds none past it.
template <size_t W>
size_t next_set(const std::array<uint64_t, W>& bits, size_t from) {
    size_t word = from / 64;
    if (word >= bits.size()) return NONE;
    uint64_t set = bits[word] & (~uint64_t{0} << (from % 64));
    while (set == 0) {
        if (++word >= bits.size()) return NONE;
        set = bits[word];
    }
    return word * 64 + static_cast<size_t>(__builtin_ctzll(set));
}

// Appends the rows the set bits fall in, ascending and without repeats, one
// block of mask at a time. Blocks arrive in stream order, only the non-empty
// ones arrive at all, and a row can span several, so the cursor carries over
// between calls. A hit's row is found by doubling the step from the cursor
// until the offsets overshoot, then binary searching that bracket. A row is
// emitted on its first hit that passes the check and the bit search resumes
// at the row's end, so its further hits are never read. A hit that fails
// moves the search on by one bit.
class GallopSeek {
   public:
    GallopSeek(const uint32_t* row_offsets, size_t offsets_len)
        : row_offsets_(row_offsets), offsets_len_(offsets_len) {}

    // `first_code` is the stream index of bit 0. The row layer covers every
    // code a bit can be set for, so a set bit always has a row.
    template <size_t W, typename Check>
    void rows(const std::array<uint64_t, W>& bits, size_t first_code, const Check& check, std::vector<size_t>& out) {
        size_t at = resolved_code_ > first_code ? resolved_code_ - first_code : 0;
        for (size_t hit; (hit = next_set(bits, at)) != NONE;) {
            size_t code = first_code + hit;
            if (code >= row_end_) {
                row_ = gallop(code);
                row_end_ = row_offsets_[row_ + 1];
            }
            if (check.passes(code, row_offsets_[row_], row_end_)) {
                out.push_back(row_);
                resolved_code_ = row_end_;
                at = row_end_ - first_code;
            } else {
                at = hit + 1;
            }
        }
    }

   private:
    // The row holding `code`, from the cursor at or before it.
    size_t gallop(size_t code) const {
        size_t step = 1;
        while (row_ + step < offsets_len_ && row_offsets_[row_ + step] <= code) step *= 2;
        size_t lo = row_ + step / 2;
        size_t hi = std::min(row_ + step, offsets_len_);
        const uint32_t* first = row_offsets_ + lo + 1;
        const uint32_t* past = std::upper_bound(first, row_offsets_ + hi, code);
        return lo + static_cast<size_t>(past - first);
    }

    const uint32_t* row_offsets_;
    size_t offsets_len_;
    size_t row_ = 0;
    // One past the last code of row_, in stream indices. Zero before the first hit.
    size_t row_end_ = 0;
    // row_end_ of the last row emitted; a bit below it is a repeat of a row
    // already out. Zero before the first emitted row.
    size_t resolved_code_ = 0;
};

}  // namespace fsst::search::prefilter::scan::resolver
