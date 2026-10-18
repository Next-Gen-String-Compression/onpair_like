#pragma once

// Two nibble-table lookups per code, ANDed: a token bit that survives both
// matched the whole code. Eight tokens per batch, batches ORed, ranges beside.
// At u8 width one batch is all the planner asks for; the bitmap in nibble.hpp
// takes over from nine tokens.

#include <array>
#include <vector>

#include "limits.hpp"
#include "range.hpp"
#include "shared.hpp"

namespace fsst::search::prefilter::scan::matcher {

struct Batch {
    Reg low;
    Reg high;
};

// Token k owns bit 1 << k in the row each of its nibbles indexes.
inline Batch batch(const uint8_t* tokens, size_t count) {
    alignas(16) uint8_t low[16] = {};
    alignas(16) uint8_t high[16] = {};
    for (size_t k = 0; k < count; ++k) {
        low[tokens[k] & 0x0f] |= static_cast<uint8_t>(1u << k);
        high[tokens[k] >> 4] |= static_cast<uint8_t>(1u << k);
    }
    return {table(low), table(high)};
}

template <size_t BATCHES>
inline V probe(const std::array<Batch, BATCHES>& batches, V codes) {
    V n0 = codes & 0x0f;
    V n1 = codes >> 4;
    V hit = lookup(batches[0].low, n0) & lookup(batches[0].high, n1);
    for (size_t b = 1; b < BATCHES; ++b)
        hit = hit | (lookup(batches[b].low, n0) & lookup(batches[b].high, n1));
    return (V)(hit != 0);
}

template <size_t BATCHES, bool SKIP_MOVEMASK_IF_NO_MATCH>
class NibbleN8 {
   public:
    explicit NibbleN8(const ProbeCover& cover) {
        const size_t n = cover.points.size();
        for (size_t b = 0; b < BATCHES; ++b) {
            size_t from = std::min(b * PER_BATCH, n);
            size_t to = std::min(from + PER_BATCH, n);
            batches_[b] = batch(cover.points.data() + from, to - from);
        }
        for (CodeRange r : cover.ranges) ranges_.push_back(hold(r));
    }

    template <size_t W>
    bool check(const uint8_t* codes, Bits<W>& bits) const {
        return words<SKIP_MOVEMASK_IF_NO_MATCH>(codes, bits, [&](V v, const uint8_t*) { return hits(v); });
    }

    // As EqOr::row.
    template <typename OnHit>
    bool row(const uint8_t* block, size_t valid, OnHit on_hit) const {
        return each_hit(hits(load(block)), valid, on_hit);
    }

   private:
    V hits(V v) const { return check_ranges(probe(batches_, v), ranges_, v); }

    std::array<Batch, BATCHES> batches_;
    std::vector<Held> ranges_;
};

}  // namespace fsst::search::prefilter::scan::matcher
