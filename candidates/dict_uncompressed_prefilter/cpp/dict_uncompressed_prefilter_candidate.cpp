// dict_uncompressed_prefilter — dictionary encoding in the ENCODED domain, with
// the plaintext code-prefilter as its child matcher. build() deduplicates the
// chunk into unique values + per-row codes; run() evaluates the prefilter once
// per unique value and scatters through the codes. Contrast the old decode-only
// `dict`, which decompressed to plaintext and scanned every row.
// See candidates/common/matcher (dict_matcher.hpp + uncompressed_matchers.hpp).

#include "dict_matcher.hpp"
#include "matcher.hpp"
#include "uncompressed_matchers.hpp"

namespace {

void* build(const lb_chunk_view* view, const char* /*config*/, char* eb, uint64_t ec) {
  auto child = std::make_unique<lb::UncompressedPrefilter>();
  return lb::adapter_build(std::make_unique<lb::DictMatcher>(std::move(child)), view, eb, ec);
}

const lb_strategy kStrategies[] = {
    {"dict+prefilter", LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY)}};

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"dict_uncompressed_prefilter",
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

extern "C" const lb_candidate* lb_candidate_dict_uncompressed_prefilter(void) {
  return &kVtable;
}
