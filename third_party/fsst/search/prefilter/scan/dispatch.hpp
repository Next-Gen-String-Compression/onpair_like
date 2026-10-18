#pragma once

// A plan as the template parameters both_stages wants.

#include <vector>

#include "matcher/eq_or.hpp"
#include "matcher/nibble.hpp"
#include "matcher/nibble_n8.hpp"
#include "matcher/range.hpp"
#include "policy/policy.hpp"
#include "resolver/gallop_seek.hpp"
#include "resolver/linear_seek.hpp"
#include "scan.hpp"

namespace fsst::search::prefilter::scan::dispatch {

// A matcher type as a value, for the lambdas below; C++17 has no type_identity.
template <typename M>
struct Kernel {
    using type = M;
};

// f(Kernel<M>{}) for the plan's matcher, compiled with the movemask skipped
// or not as the plan says.
template <typename F>
void with_matcher(policy::Plan plan, F f) {
    using namespace matcher;
    auto pick = [&](auto skip, auto keep) {
        if (plan.skip)
            f(skip);
        else
            f(keep);
    };
    switch (plan.matcher) {
        case policy::Match::EqOr: return pick(Kernel<EqOr<true>>{}, Kernel<EqOr<false>>{});
        case policy::Match::Range: return pick(Kernel<Range<true>>{}, Kernel<Range<false>>{});
        case policy::Match::NibbleN8K: return pick(Kernel<NibbleN8<1, true>>{}, Kernel<NibbleN8<1, false>>{});
        case policy::Match::Nibble: return pick(Kernel<Nibble<true>>{}, Kernel<Nibble<false>>{});
    }
}

template <typename Check>
void run(policy::Plan plan, const ProbeCover& cover, const Stream& s, const Check& check, std::vector<size_t>& out) {
    with_matcher(plan, [&](auto kernel) {
        using M = typename decltype(kernel)::type;
        if (plan.resolver == policy::Resolve::LinearSeek)
            both_stages<M, resolver::LinearSeek>(cover, s, check, out);
        else
            both_stages<M, resolver::GallopSeek>(cover, s, check, out);
    });
}

// The same plan over the rows of a selection, each on its own: no resolver.
template <typename Check>
void run_selected(policy::Plan plan, const ProbeCover& cover, const Stream& s, const uint32_t* selection,
                  size_t count, const Check& check, std::vector<size_t>& out) {
    with_matcher(plan, [&](auto kernel) {
        using M = typename decltype(kernel)::type;
        selected_rows<M>(cover, s, selection, count, check, out);
    });
}

}  // namespace fsst::search::prefilter::scan::dispatch
