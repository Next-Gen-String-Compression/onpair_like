// ac-cpp — the author's hand-written full-DFA byte-level Aho-Corasick, wrapped
// as a scanner (DESIGN.md §16). A dense 256-wide goto table per state makes
// the inner loop branch-free (one table lookup + one accept check per byte),
// the classic speed/memory trade against the aho-corasick crate's default
// NFA. Scope: contains / contains_any (the multi-pattern ops); the DFA with a
// single pattern is an ordinary contains matcher.

#include "lb_candidate.h"

#include <onpair/search/byte_aho_corasick.h>

#include <cstdint>
#include <string_view>
#include <vector>

namespace {

struct Prepared {
    ByteAhoCorasick ac;
};

void* ac_prepare(const lb_query* query) {
    if (query->needle_count == 0) return nullptr;
    std::vector<std::string_view> needles;
    needles.reserve(query->needle_count);
    for (uint32_t i = 0; i < query->needle_count; ++i) {
        const lb_bytes& n = query->needles[i];
        needles.emplace_back(reinterpret_cast<const char*>(n.ptr),
                             static_cast<size_t>(n.len));
    }
    return new Prepared{ByteAhoCorasick::build(needles)};
}

int ac_scan(void* prepared, const lb_chunk_view* view,
            uint64_t* out_bitmap_words, lb_run_stats* /*stats*/) {
    const auto& p = *static_cast<Prepared*>(prepared);
    for (uint64_t i = 0; i < view->num_rows; ++i) {
        const uint64_t start = view->offsets[i];
        const uint64_t len   = view->offsets[i + 1] - start;
        if (p.ac.scan(reinterpret_cast<const char*>(view->bytes + start),
                      static_cast<size_t>(len))) {
            out_bitmap_words[i >> 6] |= uint64_t{1} << (i & 63);
        }
    }
    return 0;
}

void ac_release(void* prepared) { delete static_cast<Prepared*>(prepared); }

const lb_scanner kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"ac-cpp",
    /*version=*/"0.1.0",
    /*cpu_features=*/nullptr,
    /*supported_ops=*/LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY),
    /*prepare=*/ac_prepare,
    /*scan=*/ac_scan,
    /*release=*/ac_release,
    /*supports_query=*/nullptr,
};

}  // namespace

extern "C" const lb_scanner* lb_scanner_ac_cpp(void) { return &kVtable; }
