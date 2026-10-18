// The resolver against the rows a mask names, worked out from the row layer
// directly, over every density and row shape.

#include <algorithm>
#include <string>
#include <vector>

#include "../tests.hpp"
#include "resolver.hpp"

using namespace fsst::search::prefilter::scan;
using namespace fsst::search::prefilter::scan::resolver;

template <typename Check>
static std::vector<size_t> resolve(const std::vector<uint64_t>& mask, const std::vector<uint32_t>& offsets,
                                   const Check& check) {
    GallopSeek resolver(offsets.data(), offsets.size());
    std::vector<size_t> out;
    for (size_t block = 0; block * (BLOCK / 64) < mask.size(); ++block) {
        Mask bits{};
        for (size_t w = 0; w < bits.size(); ++w) {
            size_t at = block * (BLOCK / 64) + w;
            bits[w] = at < mask.size() ? mask[at] : 0;
        }
        resolver.rows(bits, block * BLOCK, check, out);
    }
    return out;
}

struct RejectEven {
    bool passes(size_t code, size_t, size_t) const { return code % 2 == 1; }
};

static void resolves(const std::string& name, const std::vector<uint64_t>& mask,
                     const std::vector<uint32_t>& offsets) {
    CHECK_MSG(resolve(mask, offsets, AcceptAll{}) == rows_of_mask(mask, offsets, AcceptAll{}), "%s", name.c_str());
    CHECK_MSG(resolve(mask, offsets, RejectEven{}) == rows_of_mask(mask, offsets, RejectEven{}), "odd: %s",
              name.c_str());
}

static const size_t CODES = 3 * BLOCK + 500;

static std::vector<uint32_t> uniform(size_t per_row) {
    std::vector<uint32_t> offsets{0};
    while (offsets.back() < CODES)
        offsets.push_back(static_cast<uint32_t>(std::min(CODES, offsets.back() + per_row)));
    return offsets;
}

static std::vector<uint32_t> ragged(uint64_t seed) {
    Rng rng{seed};
    std::vector<uint32_t> offsets{0};
    while (offsets.back() < CODES) {
        size_t len = rng.below(8) == 0 ? 0 : rng.below(40) + 1;
        offsets.push_back(static_cast<uint32_t>(std::min(CODES, offsets.back() + len)));
    }
    return offsets;
}

static std::vector<uint64_t> strided(size_t stride) {
    std::vector<uint64_t> mask(CODES / 64 + 1, 0);
    for (size_t code = 0; code < CODES; code += stride) mask[code / 64] |= uint64_t{1} << (code % 64);
    return mask;
}

static std::vector<uint64_t> random_mask(uint64_t seed, unsigned one_in) {
    Rng rng{seed};
    std::vector<uint64_t> mask(CODES / 64 + 1, 0);
    for (size_t code = 0; code < CODES; ++code)
        if (rng.below(one_in) == 0) mask[code / 64] |= uint64_t{1} << (code % 64);
    return mask;
}

static std::vector<std::pair<std::string, std::vector<uint32_t>>> layers() {
    return {{"uniform 1", uniform(1)},     {"uniform 3", uniform(3)},   {"uniform 12", uniform(12)},
            {"uniform 257", uniform(257)}, {"uniform 5000", uniform(5000)}, {"ragged a", ragged(1)},
            {"ragged b", ragged(2)},       {"one row", {0, static_cast<uint32_t>(CODES)}}};
}

static void agree_on_every_density() {
    for (auto& [name, offsets] : layers()) {
        resolves(name + " empty", std::vector<uint64_t>(CODES / 64 + 1, 0), offsets);
        for (size_t stride : {size_t{1}, size_t{2}, size_t{3}, size_t{7}, size_t{64}, size_t{65}, size_t{1000}, BLOCK - 1, BLOCK, BLOCK + 1})
            resolves(name + " stride " + std::to_string(stride), strided(stride), offsets);
        for (unsigned one_in : {1u, 2u, 10u, 100u, 3000u})
            resolves(name + " random 1/" + std::to_string(one_in), random_mask(one_in, one_in),
                           offsets);
    }
}

static void a_row_hit_in_two_blocks() {
    std::vector<uint32_t> offsets{0, 100, BLOCK - 10, BLOCK + 10, BLOCK + 11, static_cast<uint32_t>(CODES)};
    std::vector<uint64_t> mask(CODES / 64 + 1, 0);
    auto set = [&](size_t code) { mask[code / 64] |= uint64_t{1} << (code % 64); };
    set(BLOCK - 5);
    set(BLOCK - 1);
    set(BLOCK);
    set(BLOCK + 9);
    set(BLOCK + 10);
    resolves("two blocks", mask, offsets);
    CHECK((resolve(mask, offsets, AcceptAll{}) == std::vector<size_t>{2, 3}));
}

static void the_ends_of_the_stream() {
    for (auto& [name, offsets] : layers()) {
        std::vector<uint64_t> mask(CODES / 64 + 1, 0);
        mask[0] |= 1;
        mask[(CODES - 1) / 64] |= uint64_t{1} << ((CODES - 1) % 64);
        resolves(name + " ends", mask, offsets);
    }
}

static void one_hit_after_many_rows() {
    std::vector<uint32_t> offsets = uniform(1);
    std::vector<uint64_t> mask(CODES / 64 + 1, 0);
    mask[(CODES - 1) / 64] |= uint64_t{1} << ((CODES - 1) % 64);
    resolves("one hit at the end of 12k rows", mask, offsets);
    CHECK((resolve(mask, offsets, AcceptAll{}) == std::vector<size_t>{CODES - 1}));
}

int main() {
    agree_on_every_density();
    a_row_hit_in_two_blocks();
    the_ends_of_the_stream();
    one_hit_after_many_rows();
    return finish("prefilter resolver tests");
}
