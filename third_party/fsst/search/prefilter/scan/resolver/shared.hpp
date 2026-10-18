#pragma once

// The loop both resolvers run; they differ only in how a hit's row is found.

#include <cstddef>
#include <cstdint>
#include <vector>

#include "../scan.hpp"

namespace fsst::search::prefilter::scan::resolver {

constexpr size_t NONE = static_cast<size_t>(-1);

// The set bit at or after `from`, NONE if the block holds none past it.
template <size_t W>
size_t next_set(const Bits<W>& bits, size_t from) {
    size_t word = from / 64;
    if (word >= bits.size()) return NONE;
    uint64_t set = bits[word] & (~uint64_t{0} << (from % 64));
    while (set == 0) {
        if (++word >= bits.size()) return NONE;
        set = bits[word];
    }
    return word * 64 + static_cast<size_t>(__builtin_ctzll(set));
}

// A row is emitted on its first hit that passes the check and the bit search
// resumes at the row's end. A hit that fails moves on by one bit.
class Cursor {
   public:
    Cursor(const uint32_t* row_offsets, size_t offsets_len) : row_offsets_(row_offsets), offsets_len_(offsets_len) {}

    // find(row_offsets, offsets_len, from_row, code) is the row holding `code`.
    template <size_t W, typename Check, typename Find>
    void rows(const Bits<W>& bits, size_t first_code, const Check& check, std::vector<size_t>& out, Find find) {
        size_t at = resolved_code_ > first_code ? resolved_code_ - first_code : 0;
        for (size_t hit; (hit = next_set(bits, at)) != NONE;) {
            size_t code = first_code + hit;
            if (code >= row_end_) {
                row_ = find(row_offsets_, offsets_len_, row_, code);
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
    const uint32_t* row_offsets_;
    size_t offsets_len_;
    size_t row_ = 0;
    size_t row_end_ = 0;
    size_t resolved_code_ = 0;
};

}  // namespace fsst::search::prefilter::scan::resolver
