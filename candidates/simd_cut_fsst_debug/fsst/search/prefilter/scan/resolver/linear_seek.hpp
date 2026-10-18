#pragma once

// One step per row crossed: what a dense mask over long rows wants.

#include "shared.hpp"

namespace fsst::search::prefilter::scan::resolver {

class LinearSeek {
   public:
    LinearSeek(const uint32_t* row_offsets, size_t offsets_len) : cursor_(row_offsets, offsets_len) {}

    template <size_t W, typename Check>
    void rows(const Bits<W>& bits, size_t first_code, const Check& check, std::vector<size_t>& out) {
        cursor_.rows(bits, first_code, check, out, walk);
    }

   private:
    static size_t walk(const uint32_t* row_offsets, size_t, size_t from, size_t code) {
        size_t row = from;
        while (row_offsets[row + 1] <= code) ++row;
        return row;
    }

    Cursor cursor_;
};

}  // namespace fsst::search::prefilter::scan::resolver
