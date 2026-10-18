#pragma once

// `code - begin <= last - begin` unsigned, ORed over R ranges; folded into
// every kernel and run alone here. K = 0, R >= 1.

#include <vector>

#include "shared.hpp"

namespace fsst::search::prefilter::scan::matcher {

struct Held {
    Reg lo;
    Reg width;
};

inline Held hold(CodeRange r) {
    return {splat(r.begin), splat(static_cast<uint8_t>(r.last - r.begin))};
}

// x86 has no unsigned byte compare; the compiler emits max plus compare.
inline V inside(Held h, V codes) {
    return (V)((codes - spread(h.lo)) <= spread(h.width));
}

inline V check_ranges(V hit, const std::vector<Held>& held, V codes) {
    for (const Held& h : held) hit = hit | inside(h, codes);
    return hit;
}

template <bool SKIP_MOVEMASK_IF_NO_MATCH>
class Range {
   public:
    explicit Range(const ProbeCover& cover) : first_(hold(cover.ranges[0])) {
        for (size_t i = 1; i < cover.ranges.size(); ++i) rest_.push_back(hold(cover.ranges[i]));
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
    V hits(V v) const { return check_ranges(inside(first_, v), rest_, v); }

    Held first_;
    std::vector<Held> rest_;
};

}  // namespace fsst::search::prefilter::scan::matcher
