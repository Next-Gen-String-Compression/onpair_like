#pragma once

// One compare per token per vector, ORed, plus the ranges, plus the pairs:
// a pair is the compare of the first code ANDed with the compare of the
// second on the codes one lane later. K + P >= 1, any R.

#include <vector>

#include "range.hpp"
#include "shared.hpp"

namespace fsst::search::prefilter::scan::matcher {

template <bool SKIP_MOVEMASK_IF_NO_MATCH>
class EqOr {
   public:
    explicit EqOr(const ProbeCover& cover) {
        for (uint8_t t : cover.points) tokens_.push_back(splat(t));
        for (CodeRange r : cover.ranges) ranges_.push_back(hold(r));
        for (CodePair p : cover.pairs) pairs_.push_back({splat(p.first), splat(p.second)});
    }

    // The pair path is chosen once per block, so a cover without pairs runs
    // the token loop alone.
    template <size_t W>
    bool check(const uint8_t* codes, Bits<W>& bits) const {
        if (pairs_.empty())
            return words<SKIP_MOVEMASK_IF_NO_MATCH>(codes, bits, [&](V v, const uint8_t*) {
                return check_ranges(token_hits(v), ranges_, v);
            });
        return words<SKIP_MOVEMASK_IF_NO_MATCH>(codes, bits, [&](V v, const uint8_t* at) {
            V hit = tokens_.empty() ? V{} : token_hits(v);
            V next = load(at + 1);
            for (const Pair& p : pairs_)
                hit = hit | ((V)(v == spread(p.first)) & (V)(next == spread(p.second)));
            return check_ranges(hit, ranges_, v);
        });
    }

   private:
    struct Pair {
        Reg first, second;
    };

    // K >= 1 here.
    V token_hits(V codes) const {
        V hit = (V)(codes == spread(tokens_[0]));
        for (size_t k = 1; k < tokens_.size(); ++k) hit = hit | (V)(codes == spread(tokens_[k]));
        return hit;
    }

    std::vector<Reg> tokens_;
    std::vector<Held> ranges_;
    std::vector<Pair> pairs_;
};

}  // namespace fsst::search::prefilter::scan::matcher
