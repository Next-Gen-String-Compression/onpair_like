// The policy against its own model, the boundaries the fit puts the kernels
// at, and the planned scan against the scalar rows.

#include <vector>

#include "../execute.hpp"
#include "../tests.hpp"

using namespace fsst::search::prefilter;
using namespace fsst::search::prefilter::scan;
using namespace fsst::search::prefilter::scan::policy;

static const char* name(Match m) {
    switch (m) {
        case Match::EqOr: return "eq_or";
        case Match::Range: return "range";
        case Match::NibbleN8K: return "nibble_n8k";
        case Match::Nibble: return "nibble";
    }
    return "?";
}

static ProbeCover probe(size_t k, size_t r) {
    ProbeCover cover;
    for (size_t i = 0; i < k; ++i) cover.points.push_back(static_cast<uint8_t>(i * 7 + 1));
    for (size_t i = 0; i < r; ++i)
        cover.ranges.push_back({static_cast<uint8_t>(200 + 6 * i), static_cast<uint8_t>(203 + 6 * i)});
    return cover;
}

static void the_planned_matcher_takes_its_cover() {
    for (size_t k = 0; k <= 64; ++k)
        for (size_t r = 0; r <= 8; ++r)
            for (size_t p = 0; p <= 4; ++p) {
                if (k == 0 && r == 0 && p == 0) continue;
                Shape s{k, r, p};
                Match m = select_matcher(s);
                CHECK_MSG(takes(m, s), "K=%zu R=%zu P=%zu planned %s", k, r, p, name(m));
                if (p > 0) CHECK(m == Match::EqOr);
                for (Match other : KERNELS)
                    if (takes(other, s)) CHECK_MSG(ns_per_code(m, s) <= ns_per_code(other, s), "K=%zu R=%zu", k, r);
            }
}

// The ladder the fitted rows give on NEON at u8 width.
static void the_model_puts_the_boundaries_where_the_sweep_says() {
    CHECK(select_matcher({1, 0}) == Match::EqOr);
    CHECK(select_matcher({2, 0}) == Match::EqOr);
    CHECK(select_matcher({3, 0}) == Match::NibbleN8K);
    CHECK(select_matcher({8, 0}) == Match::NibbleN8K);
    CHECK(select_matcher({9, 0}) == Match::Nibble);
    CHECK(select_matcher({64, 0}) == Match::Nibble);
    CHECK(select_matcher({0, 1}) == Match::Range);
    CHECK(select_matcher({0, 3}) == Match::Range);
    CHECK(select_matcher({0, 4}) == Match::Nibble);
    CHECK(select_matcher({1, 1}) == Match::EqOr);
    CHECK(select_matcher({1, 2}) == Match::EqOr);
    CHECK(select_matcher({1, 3}) == Match::Nibble);
    CHECK(select_matcher({8, 1}) == Match::NibbleN8K);
    CHECK(select_matcher({8, 2}) == Match::Nibble);
}

static void the_resolver_follows_the_seek_costs() {
    // Every row hits: the walk is cheaper than a search of no distance.
    CHECK(select_resolver({1000, 100000, 1000}) == Resolve::LinearSeek);
    // One hit in a thousand rows: the search halves its way there.
    CHECK(select_resolver({10, 1000000, 10000}) == Resolve::GallopSeek);
    // The crossover is where the two seek costs meet, near 26 rows per hit.
    double g = 2.0;
    while (seek_ns_per_row(Resolve::GallopSeek, g) >= seek_ns_per_row(Resolve::LinearSeek, g)) g *= 1.01;
    CHECK(g > 20 && g < 35);
    CHECK(select_resolver({100, 100000, static_cast<size_t>(100 * (g * 0.8))}) == Resolve::LinearSeek);
    CHECK(select_resolver({100, 100000, static_cast<size_t>(100 * (g * 1.3))}) == Resolve::GallopSeek);
}

static void the_skip_is_taken_only_on_a_sparse_mask() {
    CHECK(select(probe(1, 0), {0, 1 << 20, 1000}).skip);
    CHECK(select(probe(1, 0), {10, 1 << 20, 1000}).skip);
    CHECK(!select(probe(1, 0), {1 << 16, 1 << 20, 1000}).skip);
    CHECK(!select(probe(1, 0), {1 << 19, 1 << 20, 1000}).skip);
    CHECK(skip_ns_per_code(0.0) < 0.0);
    CHECK(skip_ns_per_code(0.5) > 0.0);
}

static void scan_cost_is_monotone_in_probes_and_coverage() {
    Region region{1 << 22, 1 << 16};
    double last = 0.0;
    for (uint32_t covered : {0u, 100u, 10000u, 100000u, 1000000u}) {
        double ns = scan_ns(probe(1, 0), covered, region);
        CHECK_MSG(ns >= last, "covered %u", covered);
        last = ns;
    }
    CHECK(scan_ns(probe(0, 0), 5, region) == 0.0);
    CHECK(scan_ns(probe(1, 0), 5, {0, 5}) == 0.0);
    CHECK(scan_ns(probe(1, 0), 1000, region) < scan_ns(probe(8, 0), 1000, region));
    CHECK(scan_ns(probe(8, 0), 1000, region) <= scan_ns(probe(64, 0), 1000, region) + 1e-9);
}

// Every plan the region and cover can produce, run end to end.
static void the_planned_scan_agrees_with_the_rows() {
    Rng rng{11};
    std::vector<uint8_t> codes(3 * BLOCK + 777);
    for (uint8_t& c : codes) c = static_cast<uint8_t>(rng.below(48));
    std::vector<uint32_t> offsets{0};
    while (offsets.back() < codes.size())
        offsets.push_back(static_cast<uint32_t>(std::min<size_t>(codes.size(), offsets.back() + rng.below(20) + 1)));
    const Stream s{codes.data(), codes.size(), offsets.data(), offsets.size()};
    for (ProbeCover cover : {probe(1, 0), probe(2, 0), probe(3, 0), probe(9, 0), probe(0, 1), probe(0, 4),
                             probe(1, 1), probe(1, 3), ProbeCover{{200}, {}, {}}, ProbeCover{{}, {{0, 47}}, {}},
                             ProbeCover{{}, {}, {{3, 4}}}, ProbeCover{{1}, {{40, 41}}, {{3, 4}, {4, 3}}}}) {
        auto want = rows_oracle(cover, s);
        for (size_t covered : {size_t{0}, size_t{5}, codes.size() / 100, codes.size() / 2}) {
            std::vector<size_t> got;
            execute(cover, s, covered, codes.size(), AcceptAll{}, got);
            CHECK_MSG(got == want, "K=%zu R=%zu covered %zu", cover.points.size(), cover.ranges.size(), covered);
        }
    }
    std::vector<size_t> got;
    execute(ProbeCover{}, s, 0, codes.size(), AcceptAll{}, got);
    CHECK(got.empty());
    std::vector<uint32_t> rowless{0};
    execute(probe(1, 0), Stream{codes.data(), 0, rowless.data(), rowless.size()}, 0, 0, AcceptAll{}, got);
    CHECK(got.empty());
}

int main() {
    the_planned_matcher_takes_its_cover();
    the_model_puts_the_boundaries_where_the_sweep_says();
    the_resolver_follows_the_seek_costs();
    the_skip_is_taken_only_on_a_sparse_mask();
    scan_cost_is_monotone_in_probes_and_coverage();
    the_planned_scan_agrees_with_the_rows();
    return finish("prefilter policy tests");
}
