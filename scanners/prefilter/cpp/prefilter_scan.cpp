// prefilter — the code-prefilter uncompressed scanner.
//
// Two-stage contains: extract a fixed-width byte *code* from the needle (the
// first 4 bytes, or the first 2 when the needle is shorter than 4), reject
// every row that cannot contain that code with the portable `prefilter.hpp`
// kernel, then confirm the full needle on the survivors with
// std::string_view::find. "row contains code" is a necessary condition for
// "row contains needle", so the prefilter is exact-safe: it never drops a real
// match, and the verify removes the code's false positives.
//
// This is the prefilter peer of the memmem baseline: same (uncompressed x
// direct) storage, but the per-row substring search is gated by the cheap
// SIMD-friendly code scan instead of running memmem on every row. It reports
// `prefilter_candidates` (rows surviving the code scan) in instrumented mode.
//
// contains-family only. Longer codes are more selective, so the widest code
// the needle affords is used; a 0- or 1-byte needle yields no code and every
// row falls through to the verify (a plain find, no prefilter benefit).

#include "lb_candidate.h"
#include "prefilter.hpp"

#include <cstring>
#include <string>
#include <string_view>
#include <vector>

namespace {

// A byte code extracted from a needle: `width` is 4, 2, or 0 (no usable code).
struct Code {
  uint32_t width;
  uint32_t bytes;  // little-endian raw bytes, compared via prefilter.hpp
};

struct Prepared {
  uint32_t op;
  std::vector<std::string> needles;
  std::vector<Code> codes;   // one per needle, parallel to `needles`
  bool match_all;            // an empty needle => every row matches
};

inline Code extract_code(const std::string& n) {
  // Take the code from the *tail* of the needle: on URLs the leading bytes
  // share common prefixes (http, www, /) and make a weak filter, so the last
  // 4 (or last 2) bytes are usually more selective. Correctness is unaffected
  // by which contiguous window we pick.
  if (n.size() >= 4) {
    uint32_t c;
    std::memcpy(&c, n.data() + n.size() - 4, 4);
    return {4, c};
  }
  if (n.size() >= 2) {
    uint16_t c;
    std::memcpy(&c, n.data() + n.size() - 2, 2);
    return {2, static_cast<uint32_t>(c)};
  }
  return {0, 0};
}

inline std::string_view sv(const uint8_t* ptr, uint32_t len) {
  return {reinterpret_cast<const char*>(ptr), static_cast<size_t>(len)};
}

// Necessary-condition test: can this row hold the code? A 0-width code cannot
// prefilter (needle too short), so everything passes through to the verify.
inline bool passes(const Code& c, const char* row, uint32_t len) {
  if (c.width == 4) return prefilter::contains<uint32_t>(row, len, c.bytes);
  if (c.width == 2) return prefilter::contains<uint16_t>(row, len, static_cast<uint16_t>(c.bytes));
  return true;
}

inline void set_bit(uint64_t* words, uint64_t i) {
  words[i >> 6] |= uint64_t{1} << (i & 63);
}

void* prefilter_prepare(const lb_query* query) {
  auto* p = new Prepared;
  p->op = query->op;
  p->match_all = false;
  p->needles.reserve(query->needle_count);
  p->codes.reserve(query->needle_count);
  for (uint32_t i = 0; i < query->needle_count; i++) {
    const lb_bytes& n = query->needles[i];
    p->needles.emplace_back(reinterpret_cast<const char*>(n.ptr), static_cast<size_t>(n.len));
    p->codes.push_back(extract_code(p->needles.back()));
    if (p->needles.back().empty()) p->match_all = true;
  }
  return p;
}

int prefilter_scan(void* prepared, const lb_chunk_view* view, uint64_t* out, lb_run_stats* stats) {
  const auto& p = *static_cast<Prepared*>(prepared);
  const uint64_t n = view->num_rows;

  // '%%' (an empty fragment) matches unconditionally: no prefilter, no verify.
  if (p.match_all) {
    for (uint64_t i = 0; i < n; i++) set_bit(out, i);
    if (stats) stats->prefilter_candidates = n;
    return 0;
  }

  uint64_t candidates = 0;
  if (p.op == LB_CONTAINS) {
    const std::string& needle = p.needles[0];
    const Code& code = p.codes[0];
    for (uint64_t i = 0; i < n; i++) {
      const uint64_t start = view->offsets[i];
      const uint32_t len = static_cast<uint32_t>(view->offsets[i + 1] - start);
      const char* row = reinterpret_cast<const char*>(view->bytes) + start;
      const bool pf = passes(code, row, len);
      candidates += pf;
      if (pf && sv(view->bytes + start, len).find(needle) != std::string_view::npos) {
        set_bit(out, i);
      }
    }
  } else if (p.op == LB_CONTAINS_ANY) {
    const uint32_t k = static_cast<uint32_t>(p.needles.size());
    for (uint64_t i = 0; i < n; i++) {
      const uint64_t start = view->offsets[i];
      const uint32_t len = static_cast<uint32_t>(view->offsets[i + 1] - start);
      const char* row = reinterpret_cast<const char*>(view->bytes) + start;
      const std::string_view s = sv(view->bytes + start, len);
      bool hit = false;
      for (uint32_t j = 0; j < k && !hit; j++) {
        if (!passes(p.codes[j], row, len)) continue;
        candidates++;
        hit = s.find(p.needles[j]) != std::string_view::npos;
      }
      if (hit) set_bit(out, i);
    }
  } else {
    return 1;  // unsupported op reached scan (should be gated by supported_ops)
  }

  if (stats) stats->prefilter_candidates = candidates;
  return 0;
}

void prefilter_release(void* prepared) { delete static_cast<Prepared*>(prepared); }

const lb_scanner kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"prefilter",
    /*version=*/"0.1.0",
    /*cpu_features=*/nullptr,
    /*supported_ops=*/LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY),
    /*prepare=*/prefilter_prepare,
    /*scan=*/prefilter_scan,
    /*release=*/prefilter_release,
    /*supports_query=*/nullptr,
};

}  // namespace

extern "C" const lb_scanner* lb_scanner_prefilter(void) { return &kVtable; }
