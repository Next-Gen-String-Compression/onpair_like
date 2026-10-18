// Every matcher against the scalar definition of the mask, and the block
// driver against a per-row oracle.

#include <algorithm>
#include <set>
#include <vector>

#include "../resolver/gallop_seek.hpp"
#include "../resolver/linear_seek.hpp"
#include "../tests.hpp"
#include "eq_or.hpp"
#include "nibble.hpp"
#include "nibble_n8.hpp"
#include "range.hpp"

using namespace fsst::search::prefilter;
using namespace fsst::search::prefilter::scan;
using namespace fsst::search::prefilter::scan::matcher;

// The kernel reads one code past the block for a pair's second.
static Mask expected(const ProbeCover& cover, const uint8_t* codes) {
    Mask m{};
    for (size_t i = 0; i < BLOCK; ++i)
        if (cover_matches(cover, codes, i, BLOCK + 1)) m[i / 64] |= uint64_t{1} << (i % 64);
    return m;
}

static bool all_zero(const Mask& m) {
    return std::all_of(m.begin(), m.end(), [](uint64_t w) { return w == 0; });
}

template <typename M>
static void agrees(const char* name, const ProbeCover& cover, const uint8_t* codes) {
    M matcher(cover);
    Mask bits;
    bits.fill(~uint64_t{0});
    bool may = matcher.check(codes, bits);
    Mask want = expected(cover, codes);
    CHECK_MSG(bits == want, "%s: mask differs (K=%zu R=%zu P=%zu)", name, cover.points.size(), cover.ranges.size(),
              cover.pairs.size());
    if (!may) CHECK_MSG(all_zero(want), "%s: promised an empty mask but the cover hits", name);
}

static void every_matcher(const ProbeCover& cover, const uint8_t* codes) {
    const size_t k = cover.points.size();
    const size_t r = cover.ranges.size();
    const size_t p = cover.pairs.size();
    if (k > 0 || p > 0) {
        agrees<EqOr<true>>("eq_or skip", cover, codes);
        agrees<EqOr<false>>("eq_or", cover, codes);
    }
    if (p > 0) return;
    agrees<Nibble<true>>("nibble skip", cover, codes);
    agrees<Nibble<false>>("nibble", cover, codes);
    if (k == 0 && r > 0) {
        agrees<Range<true>>("range skip", cover, codes);
        agrees<Range<false>>("range", cover, codes);
    }
    if (k >= 1 && k <= PER_BATCH) {
        agrees<NibbleN8<1, true>>("nibble_n8 skip", cover, codes);
        agrees<NibbleN8<1, false>>("nibble_n8", cover, codes);
    }
}

static Block block(uint64_t seed) {
    Rng rng{seed};
    Block b;
    for (uint8_t& c : b) c = static_cast<uint8_t>(rng.next());
    return b;
}

static std::vector<uint8_t> distinct_codes(const Block& b, size_t count) {
    std::vector<uint8_t> out;
    std::set<uint8_t> seen;
    for (size_t i = 0; out.size() < count && i < BLOCK; ++i)
        if (seen.insert(b[i]).second) out.push_back(b[i]);
    return out;
}

static void needles_the_block_holds() {
    Block codes = block(1);
    for (size_t count : {1, 2, 3, 4, 8, 9, 12, 16, 17, 24, 64}) {
        ProbeCover cover{distinct_codes(codes, count), {}};
        CHECK(cover.points.size() == count);
        CHECK(!all_zero(expected(cover, codes.data())));
        every_matcher(cover, codes.data());
        cover.ranges = {{200, 210}};
        every_matcher(cover, codes.data());
        cover.ranges = {{200, 210}, {3, 3}, {0, 40}};
        every_matcher(cover, codes.data());
    }
}

static void needles_the_block_does_not_hold() {
    Block codes;
    codes.fill(7);
    every_matcher(ProbeCover{{60, 61, 62, 63}, {}}, codes.data());
    every_matcher(ProbeCover{{}, {{100, 200}}}, codes.data());
    every_matcher(ProbeCover{{60}, {{100, 200}, {0, 6}, {8, 255}}}, codes.data());
}

static void a_hit_at_the_seam() {
    Block codes;
    codes.fill(7);
    codes[BLOCK - 1] = 200;
    ProbeCover cover{{200}, {}};
    CHECK(expected(cover, codes.data())[BLOCK / 64 - 1] == uint64_t{1} << 63);
    every_matcher(cover, codes.data());
    codes[0] = 200;
    every_matcher(cover, codes.data());
}

// Codes sharing one nibble with a token must not match it.
static void nibble_cross_products() {
    Block codes;
    codes.fill(0x77);
    codes[5] = 0x12;
    codes[6] = 0x14;
    codes[7] = 0x32;
    codes[8] = 0x34;
    codes[9] = 0x31;
    codes[10] = 0x24;
    ProbeCover cover{{0x12, 0x34}, {}};
    CHECK(expected(cover, codes.data())[0] == (uint64_t{1} << 5 | uint64_t{1} << 8));
    every_matcher(cover, codes.data());
}

static void both_halves_of_the_bitmap() {
    Block codes = block(2);
    every_matcher(ProbeCover{{0x0f, 0x8f, 0x70, 0xf0, 0x00, 0xff, 0x7f, 0x80}, {}}, codes.data());
    every_matcher(ProbeCover{{0x80}, {}}, codes.data());
    every_matcher(ProbeCover{{0x7f}, {}}, codes.data());
}

static void ranges_of_codes() {
    Block codes = block(3);
    for (CodeRange r : {CodeRange{0, 255}, CodeRange{0, 0}, CodeRange{37, 37}, CodeRange{100, 200},
                        CodeRange{250, 255}, CodeRange{128, 128}, CodeRange{127, 128},
                        CodeRange{0, 127}, CodeRange{128, 255}}) {
        every_matcher(ProbeCover{{}, {r}}, codes.data());
        every_matcher(ProbeCover{{5, 130}, {r}}, codes.data());
        every_matcher(ProbeCover{{}, {r, {3, 4}}}, codes.data());
    }
}

// Pairs alone, beside tokens, at the block seam and reading the padding.
static void pairs_of_codes() {
    Block codes = block(4);
    for (size_t i = 0; i < BLOCK; i += 97) codes[i] = 0x21, codes[i + 1] = 0x22;
    codes[BLOCK - 1] = 0x21;
    codes[BLOCK] = 0x22;
    ProbeCover pair{{}, {}, {{0x21, 0x22}}};
    CHECK(expected(pair, codes.data())[BLOCK / 64 - 1] >> 63 == 1);
    every_matcher(pair, codes.data());
    every_matcher(ProbeCover{{0x50}, {}, {{0x21, 0x22}}}, codes.data());
    every_matcher(ProbeCover{{}, {{0x60, 0x6f}}, {{0x21, 0x22}, {0x22, 0x21}}}, codes.data());
    every_matcher(ProbeCover{{}, {}, {{0x21, 0x21}}}, codes.data());
    codes[BLOCK] = 0x23;
    CHECK(expected(pair, codes.data())[BLOCK / 64 - 1] >> 63 == 0);
    every_matcher(pair, codes.data());
}

static void random_covers() {
    Rng rng{99};
    for (int trial = 0; trial < 200; ++trial) {
        Block codes = block(1000 + trial);
        ProbeCover cover;
        size_t k = rng.below(41);
        size_t pairs = trial % 3 == 0 ? rng.below(4) : 0;
        for (size_t i = 0; i < pairs; ++i)
            cover.pairs.push_back({static_cast<uint8_t>(rng.next()), static_cast<uint8_t>(rng.next())});
        std::set<uint8_t> seen;
        while (cover.points.size() < k) {
            uint8_t c = static_cast<uint8_t>(rng.next());
            if (seen.insert(c).second) cover.points.push_back(c);
        }
        size_t r = rng.below(4);
        for (size_t i = 0; i < r; ++i) {
            uint8_t a = static_cast<uint8_t>(rng.next());
            uint8_t b = static_cast<uint8_t>(rng.next());
            cover.ranges.push_back({std::min(a, b), std::max(a, b)});
        }
        if (cover.empty()) cover.points.push_back(1);
        if (!cover.pairs.empty() && cover.points.empty() && rng.below(2)) cover.points.clear();
        every_matcher(cover, codes.data());
    }
}

// The driver over whole blocks and a tail, against a per-row oracle.

static std::vector<uint32_t> ragged_offsets(size_t total, Rng& rng) {
    std::vector<uint32_t> offsets{0};
    size_t at = 0;
    while (at < total) {
        size_t len = rng.below(10) == 0 ? 0 : rng.below(30) + 1;
        at = std::min(total, at + len);
        offsets.push_back(static_cast<uint32_t>(at));
    }
    return offsets;
}

template <typename M, typename R>
static void driver_agrees(const char* name, const ProbeCover& cover, const std::vector<uint8_t>& codes,
                          const std::vector<uint32_t>& offsets) {
    const Stream s{codes.data(), codes.size(), offsets.data(), offsets.size()};
    std::vector<size_t> got;
    both_stages<M, R>(cover, s, AcceptAll{}, got);
    CHECK_MSG(got == rows_oracle(cover, s), "%s: rows differ over %zu codes", name, codes.size());
}

// A pair whose first code is the last of a whole block and whose second
// opens the next block: the driver must keep that bit, and drop only the
// stream's last position when it pairs with padding.
static void a_pair_across_the_block_seam() {
    for (size_t len : {2 * BLOCK, 2 * BLOCK + 1, 3 * BLOCK + 5}) {
        std::vector<uint8_t> codes(len, 0);
        codes[BLOCK - 1] = 3;
        codes[BLOCK] = 5;
        codes[len - 1] = 3;  // pairs with the padding zero: no pair hit
        std::vector<uint32_t> offsets{0, static_cast<uint32_t>(BLOCK - 1), static_cast<uint32_t>(BLOCK + 1),
                                      static_cast<uint32_t>(len - 1), static_cast<uint32_t>(len)};
        ProbeCover pair{{}, {}, {{3, 5}}};
        driver_agrees<EqOr<false>, resolver::LinearSeek>("pair at the seam", pair, codes, offsets);
        ProbeCover zero_second{{}, {}, {{3, 0}}};
        driver_agrees<EqOr<false>, resolver::LinearSeek>("pair with the padding", zero_second, codes,
                                                                   offsets);
        std::vector<size_t> got;
        both_stages<EqOr<false>, resolver::LinearSeek>(pair, Stream{codes.data(), len, offsets.data(), offsets.size()},
                                                       AcceptAll{}, got);
        CHECK(got == std::vector<size_t>{1});
    }
}

static void the_driver_scans_every_block() {
    Rng rng{7};
    for (size_t len : {size_t{0}, size_t{1}, size_t{10}, size_t{63}, size_t{64}, BLOCK - 1, BLOCK,
                       BLOCK + 1, 3 * BLOCK + 123}) {
        std::vector<uint8_t> codes(len);
        for (uint8_t& c : codes) c = static_cast<uint8_t>(rng.below(64));
        std::vector<uint32_t> offsets = ragged_offsets(len, rng);
        for (ProbeCover cover : {ProbeCover{{7}, {}, {}}, ProbeCover{{7, 9, 11}, {}, {}},
                                 ProbeCover{{}, {{20, 25}}, {}}, ProbeCover{{1}, {{20, 25}, {60, 63}}, {}},
                                 ProbeCover{{200}, {}, {}}, ProbeCover{{}, {}, {{3, 5}}},
                                 ProbeCover{{}, {}, {{codes.empty() ? uint8_t{0} : codes.back(), 0}}},
                                 ProbeCover{{9}, {{40, 41}}, {{3, 5}, {5, 3}}}}) {
            if (!cover.pairs.empty()) {
                driver_agrees<EqOr<true>, resolver::GallopSeek>("eq_or pairs gallop", cover, codes, offsets);
                driver_agrees<EqOr<false>, resolver::LinearSeek>("eq_or pairs linear", cover, codes, offsets);
                continue;
            }
            driver_agrees<Nibble<true>, resolver::GallopSeek>("nibble gallop", cover, codes,
                                                                        offsets);
            driver_agrees<Nibble<false>, resolver::LinearSeek>("nibble linear", cover, codes,
                                                                         offsets);
            if (!cover.points.empty()) {
                driver_agrees<EqOr<true>, resolver::GallopSeek>("eq_or gallop", cover, codes,
                                                                          offsets);
                driver_agrees<NibbleN8<1, false>, resolver::LinearSeek>("nibble_n8 linear",
                                                                                  cover, codes, offsets);
            } else {
                driver_agrees<Range<false>, resolver::GallopSeek>("range gallop", cover, codes,
                                                                            offsets);
            }
        }
    }
}

// The tail block is zero padded; a cover holding code 0 must not see it.
static void padding_makes_no_candidate() {
    std::vector<uint8_t> codes(10, 7);
    std::vector<uint32_t> offsets{0, 10};
    const Stream s{codes.data(), codes.size(), offsets.data(), offsets.size()};
    std::vector<size_t> got;
    both_stages<Nibble<true>, resolver::LinearSeek>(ProbeCover{{0}, {}}, s, AcceptAll{}, got);
    CHECK(got.empty());
    both_stages<Nibble<false>, resolver::LinearSeek>(ProbeCover{{}, {{0, 3}}}, s, AcceptAll{}, got);
    CHECK(got.empty());
    codes[9] = 0;
    both_stages<EqOr<true>, resolver::GallopSeek>(ProbeCover{{0}, {}}, s, AcceptAll{}, got);
    CHECK((got == std::vector<size_t>{0}));
}

int main() {
    needles_the_block_holds();
    needles_the_block_does_not_hold();
    a_hit_at_the_seam();
    nibble_cross_products();
    both_halves_of_the_bitmap();
    ranges_of_codes();
    pairs_of_codes();
    random_covers();
    a_pair_across_the_block_seam();
    the_driver_scans_every_block();
    padding_makes_no_candidate();
    return finish("prefilter matcher tests");
}
