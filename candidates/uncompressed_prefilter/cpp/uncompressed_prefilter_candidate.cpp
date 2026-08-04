// uncompressed_prefilter — the plaintext code-prefilter as a self-contained
// candidate. Stores the chunk zero-copy (ratio 1.0); answers CONTAINS /
// CONTAINS_ANY by extracting a 2/4-byte tail code from the needle, rejecting
// rows that cannot hold it with prefilter.hpp, and verifying survivors with
// find(). See candidates/common/matcher.

#include "matcher.hpp"
#include "uncompressed_matchers.hpp"

namespace {

void* build(const lb_chunk_view* view, const char* /*config*/, char* eb, uint64_t ec) {
  return lb::adapter_build(std::make_unique<lb::UncompressedPrefilter>(), view, eb, ec);
}

const lb_strategy kStrategies[] = {
    {"prefilter", LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY)}};

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"uncompressed_prefilter",
    /*version=*/"0.1.0",
    /*cpu_features=*/nullptr,
    /*strategies=*/kStrategies,
    /*strategy_count=*/1,
    /*build=*/build,
    /*footprint=*/lb::adapter_footprint,
    /*run=*/lb::adapter_run,
    /*view=*/nullptr,
    /*decode=*/nullptr,
    /*destroy=*/lb::adapter_destroy,
};

}  // namespace

extern "C" const lb_candidate* lb_candidate_uncompressed_prefilter(void) { return &kVtable; }
