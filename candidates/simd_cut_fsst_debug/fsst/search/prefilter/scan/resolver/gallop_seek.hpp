#pragma once

// Double the step from the cursor until the offsets overshoot, then binary
// search that bracket: what a sparse mask over short rows wants.

#include <algorithm>

#include "shared.hpp"

namespace fsst::search::prefilter::scan::resolver {

class GallopSeek {
   public:
    GallopSeek(const uint32_t* row_offsets, size_t offsets_len) : cursor_(row_offsets, offsets_len) {}

    template <size_t W, typename Check>
    void rows(const Bits<W>& bits, size_t first_code, const Check& check, std::vector<size_t>& out) {
        cursor_.rows(bits, first_code, check, out, gallop);
    }

   private:
    static size_t gallop(const uint32_t* row_offsets, size_t offsets_len, size_t from, size_t code) {
        size_t step = 1;
        while (from + step < offsets_len && row_offsets[from + step] <= code) step *= 2;
        size_t lo = from + step / 2;
        size_t hi = std::min(from + step, offsets_len);
        const uint32_t* first = row_offsets + lo + 1;
        const uint32_t* past = std::upper_bound(first, row_offsets + hi, code);
        return lo + static_cast<size_t>(past - first);
    }

    Cursor cursor_;
};

}  // namespace fsst::search::prefilter::scan::resolver
