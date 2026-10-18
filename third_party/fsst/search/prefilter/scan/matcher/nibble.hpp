#pragma once

// The universal 16x16 bitmap of the cover, addressed by nibble. Any K, any
// R, flat cost: ranges are set into the tables and the kernel never sees them.
// `low` holds the rows for high nibbles 0..7, `high` those for 8..15.

#include "shared.hpp"

namespace fsst::search::prefilter::scan::matcher {

template <bool SKIP_MOVEMASK_IF_NO_MATCH>
class Nibble {
   public:
    explicit Nibble(const ProbeCover& cover) {
        alignas(16) uint8_t low[16] = {};
        alignas(16) uint8_t high[16] = {};
        auto set = [&](uint8_t code) {
            uint8_t row = code & 0x0f;
            uint8_t bit = code >> 4;
            if (bit < 8)
                low[row] |= static_cast<uint8_t>(1u << bit);
            else
                high[row] |= static_cast<uint8_t>(1u << (bit - 8));
        };
        for (uint8_t t : cover.points) set(t);
        for (CodeRange r : cover.ranges)
            for (unsigned c = r.begin; c <= r.last; ++c) set(static_cast<uint8_t>(c));
        low_ = table(low);
        high_ = table(high);
    }

    template <size_t W>
    bool check(const uint8_t* codes, Bits<W>& bits) const {
        // 1 << (high nibble % 8), indexed by the whole high nibble
        const Reg bit = table(LANE_BIT);
        return words<SKIP_MOVEMASK_IF_NO_MATCH>(codes, bits, [&](V v, const uint8_t*) { return hits(v, bit); });
    }

    // As EqOr::row.
    template <typename OnHit>
    bool row(const uint8_t* block, size_t valid, OnHit on_hit) const {
        return each_hit(hits(load(block), table(LANE_BIT)), valid, on_hit);
    }

   private:
    V hits(V v, Reg bit) const {
        // bit 7 kept in the index: the lookup returns 0 for the wrong half.
        V index = v & 0x8f;
        V rows = lookup(low_, index) | lookup(high_, index ^ 0x80);
        return (V)((rows & lookup(bit, v >> 4)) != 0);
    }

    Reg low_;
    Reg high_;
};

}  // namespace fsst::search::prefilter::scan::matcher
