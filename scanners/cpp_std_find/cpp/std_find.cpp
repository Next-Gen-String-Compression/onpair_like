// cpp_std_find — the C++ reference scanner (DESIGN.md §11 step 6).
//
// std::string_view::find and friends: byte-exact, no explicit SIMD (the
// standard library does whatever it does). Its job is to prove the C++
// scanner path end-to-end and to serve as an honest "what you get for
// free in C++" datum. No prefilter stage; reports no stats.

#include "lb_candidate.h"

#include <string>
#include <string_view>
#include <vector>

namespace {

struct Prepared {
  uint32_t op;
  std::vector<std::string> needles;
};

inline std::string_view sv(const uint8_t* ptr, uint64_t len) {
  return {reinterpret_cast<const char*>(ptr), static_cast<size_t>(len)};
}

bool matches(const Prepared& p, std::string_view row) {
  switch (p.op) {
    case LB_PREFIX: {
      const auto& n = p.needles[0];
      return row.size() >= n.size() && row.substr(0, n.size()) == n;
    }
    case LB_SUFFIX: {
      const auto& n = p.needles[0];
      return row.size() >= n.size() &&
             row.substr(row.size() - n.size()) == n;
    }
    case LB_CONTAINS:
      return row.find(p.needles[0]) != std::string_view::npos;
    case LB_MULTI_CONTAINS: {
      size_t pos = 0;
      for (const auto& n : p.needles) {
        const size_t found = row.find(n, pos);
        if (found == std::string_view::npos) return false;
        pos = found + n.size();
      }
      return true;
    }
    case LB_CONTAINS_ANY: {
      for (const auto& n : p.needles) {
        if (row.find(n) != std::string_view::npos) return true;
      }
      return false;
    }
    default:
      return false;
  }
}

void* std_find_prepare(const lb_query* query) {
  auto* p = new Prepared;
  p->op = query->op;
  p->needles.reserve(query->needle_count);
  for (uint32_t i = 0; i < query->needle_count; i++) {
    const lb_bytes& n = query->needles[i];
    p->needles.emplace_back(reinterpret_cast<const char*>(n.ptr),
                            static_cast<size_t>(n.len));
  }
  return p;
}

int std_find_scan(void* prepared, const lb_chunk_view* view,
                  uint64_t* out_bitmap_words, lb_run_stats* /*stats*/) {
  const auto& p = *static_cast<Prepared*>(prepared);
  for (uint64_t i = 0; i < view->num_rows; i++) {
    const uint64_t start = view->offsets[i];
    if (matches(p, sv(view->bytes + start, view->offsets[i + 1] - start))) {
      out_bitmap_words[i >> 6] |= uint64_t{1} << (i & 63);
    }
  }
  return 0;
}

void std_find_release(void* prepared) { delete static_cast<Prepared*>(prepared); }

const lb_scanner kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"cpp_std_find",
    /*version=*/"0.1.0",
    /*cpu_features=*/nullptr,
    /*supported_ops=*/LB_ALL_OPS,
    /*prepare=*/std_find_prepare,
    /*scan=*/std_find_scan,
    /*release=*/std_find_release,
    /*supports_query=*/nullptr,
};

}  // namespace

extern "C" const lb_scanner* lb_scanner_cpp_std_find(void) { return &kVtable; }
