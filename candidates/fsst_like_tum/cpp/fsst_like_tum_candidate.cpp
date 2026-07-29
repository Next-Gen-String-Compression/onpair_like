// fsst_like_tum — FSST-LIKE-Matching (calin2110/FSST-LIKE-Matching, DaMoN'26;
// pinned in CMakeLists.txt): LIKE/substring predicates evaluated directly on
// FSST-compressed bytes via a per-pattern finite automaton (DESIGN.md §17).
//
// Answers queries ONE way — the interpreted match-in-place automaton: candidate
// strategy "interp" builds a LikePatternAutomatonParser for the query, then
// drives parse() over each row's compressed bytes. (C++-codegen / LLVM-JIT
// backends land next.) This match-in-place path is the candidate's whole point,
// so it deliberately exposes NO decode(): it is a matcher, not a codec, and the
// decode-then-scan baseline is the `fsst` candidate's job.
//
// build() and footprint() are the SHARED fsst_common front-end
// (candidates/fsst_common/fsst_build.hpp) — identical to the `fsst` candidate.
// This file's cand_build() just calls fsst_common::Build() and then does the one
// extra, fork-specific step the matcher needs: write the escaped-byte bitmap
// into symbols[255] (which the matcher's isEscapable() reads) and build the
// FSST-LIKE Encoder over the trained table. The FSST fork here (calin2110) is
// DISTINCT from the `fsst` candidate's cwida upstream; both statically link a
// full FSST copy, so this candidate's FSST symbols are localized at link (see
// CMakeLists.txt) to avoid a duplicate-symbol clash.
//
// Op -> LIKE pattern, escaping % _ \ inside needles so only the implemented
// StringPattern (start/middle/end/full) path is used, never the unimplemented
// UnderscorePattern (unescaped _). Known limitation: a needle ending in a
// literal backslash yields "...\\%", which the parser's end-detection mis-reads
// as an escaped % — such needles (rare in real columns) are matched wrong; the
// correctness gate is the backstop. CONTAINS_ANY is unsupported (an OR of
// literals is not one LIKE pattern).

#include "lb_candidate.h"

// libfsst.hpp has NO include guard; encoder.hpp includes it exactly once. Never
// include <fsst/libfsst.hpp> directly here. fsst.h IS guarded (FSST_INCLUDED_H).
#include "encoder.hpp"
#include "like_pattern_automaton.hpp"
#include <fsst/fsst.h>

#include "fsst_build.hpp"  // shared front-end; include AFTER <fsst/fsst.h>

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <memory>
#include <span>
#include <string>
#include <vector>

namespace {

// The shared FSST build result, plus the one fork-specific extra this candidate
// needs: the FSST-LIKE Encoder (over the clobbered table) that drives matching.
struct Handle {
  fsst_common::FsstBuilt b;
  std::unique_ptr<Encoder> encoder;
};

// Append `nd` to `out` with LIKE metacharacters escaped, so it is matched as a
// literal substring (never a wildcard).
void escape_append(std::vector<uint8_t>& out, const lb_bytes& nd) {
  for (uint64_t i = 0; i < nd.len; i++) {
    const uint8_t c = nd.ptr[i];
    if (c == '%' || c == '_' || c == '\\') out.push_back('\\');
    out.push_back(c);
  }
}

// (op, needles) -> LIKE pattern bytes. Returns false for CONTAINS_ANY (not one
// LIKE pattern) — never sent, since it is absent from supported_ops.
bool to_like_pattern(const lb_query* q, std::vector<uint8_t>& pat) {
  auto nd = [q](uint32_t i) { return q->needles[i]; };
  switch (q->op) {
    case LB_PREFIX:
      escape_append(pat, nd(0));
      pat.push_back('%');
      return true;
    case LB_SUFFIX:
      pat.push_back('%');
      escape_append(pat, nd(0));
      return true;
    case LB_CONTAINS:
      pat.push_back('%');
      escape_append(pat, nd(0));
      pat.push_back('%');
      return true;
    case LB_MULTI_CONTAINS:
      pat.push_back('%');
      for (uint32_t i = 0; i < q->needle_count; i++) {
        escape_append(pat, nd(i));
        pat.push_back('%');
      }
      return true;
    default:
      return false;
  }
}

void* cand_build(const lb_chunk_view* view, const char* /*config_json*/,
                 char* err_buf, uint64_t err_cap) {
  auto fail = [&](const char* msg) -> void* {
    if (err_cap > 0) std::snprintf(err_buf, err_cap, "%s", msg);
    return nullptr;
  };

  auto h = std::make_unique<Handle>();
  // Shared front-end: train + compress + concatenate + export. Leaves h->b.enc
  // alive for the automaton construction below.
  if (!fsst_common::Build(view, h->b, err_buf, err_cap)) return nullptr;

  try {
    // The libfsst::SymbolTable this encoder trained. Shared: keeping a copy of
    // the shared_ptr keeps it alive after fsst_destroy(enc).
    std::shared_ptr<libfsst::SymbolTable> sym =
        reinterpret_cast<libfsst::Encoder*>(h->b.enc)->symbolTable;

    // Escaped-byte bitmap into symbols[255], exactly as the repo's compressFile:
    // isEscapable(b) reads reinterpret_cast<bool*>(&symbols[255])[b]. Scan each
    // compressed row (escape 255 is always followed by its literal within the
    // same row, so the concatenated stream reads identically to per-row).
    bool bitmap[256] = {false};
    for (uint64_t i = 0; i < h->b.num_rows; i++) {
      const uint8_t* p = h->b.compressed.data() + h->b.coffsets[i];
      const size_t len = size_t(h->b.coffsets[i + 1] - h->b.coffsets[i]);
      size_t j = 0;
      while (j < len) {
        if (p[j] == 255) { ++j; bitmap[p[j]] = true; }
        ++j;
      }
    }
    std::memcpy(&sym->symbols[255], bitmap, 256 * sizeof(bool));

    // FSST-LIKE Encoder over the (clobbered) table, for automaton construction.
    SymbolTable st(sym);
    h->encoder = std::make_unique<Encoder>(st);

    fsst_destroy(h->b.enc);
    h->b.enc = nullptr;
    return h.release();
  } catch (const std::exception& e) {
    if (h->b.enc) fsst_destroy(h->b.enc);
    return fail((std::string("build failed: ") + e.what()).c_str());
  } catch (...) {
    if (h->b.enc) fsst_destroy(h->b.enc);
    return fail("build failed: unknown exception");
  }
}

uint32_t cand_footprint(void* self, lb_footprint_component* out,
                        uint32_t capacity) {
  return fsst_common::Footprint(static_cast<Handle*>(self)->b, out, capacity);
}

// "interp" strategy: build the automaton for this query (per-query setup,
// self-timed into setup_ns like a scanner prepare()), then drive it over the
// compressed rows. No memoization across calls (SEMANTICS.md rule 1).
int cand_run(void* self, uint32_t strategy_index, const lb_query* query,
             uint64_t* out_bitmap_words, lb_run_stats* stats_or_null) {
  auto* h = static_cast<Handle*>(self);
  if (strategy_index != 0) return 10;

  std::vector<uint8_t> pat;
  if (!to_like_pattern(query, pat)) return 11;  // e.g. CONTAINS_ANY (unsupported)

  auto set_bit = [out_bitmap_words](uint64_t row) {
    out_bitmap_words[row >> 6] |= uint64_t(1) << (row & 63);
  };
  using Clock = std::chrono::steady_clock;
  const auto setup_start = stats_or_null ? Clock::now() : Clock::time_point{};

  try {
    automata::parsing::LikePatternAutomatonParser parser(
        std::span<const uint8_t>(pat.data(), pat.size()), *h->encoder);
    if (stats_or_null) {
      stats_or_null->setup_ns = uint64_t(
          std::chrono::duration_cast<std::chrono::nanoseconds>(Clock::now() -
                                                               setup_start)
              .count());
    }
    for (uint64_t i = 0; i < h->b.num_rows; i++) {
      const uint8_t* p = h->b.compressed.data() + h->b.coffsets[i];
      const size_t len = size_t(h->b.coffsets[i + 1] - h->b.coffsets[i]);
      if (parser.parse(std::span<const uint8_t>(p, len))) set_bit(i);
    }
    return 0;
  } catch (...) {
    return 12;
  }
}

void cand_destroy(void* self) { delete static_cast<Handle*>(self); }

const lb_strategy kStrategies[] = {
    {"interp", LB_OP_BIT(LB_PREFIX) | LB_OP_BIT(LB_SUFFIX) |
                   LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_MULTI_CONTAINS)},
};

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"fsst_like_tum",
    /*version=*/"0.1.0+b1eb3ab",
    /*cpu_features=*/nullptr,
    /*strategies=*/kStrategies,
    /*strategy_count=*/1,
    /*build=*/cand_build,
    /*footprint=*/cand_footprint,
    /*run=*/cand_run,
    /*view=*/nullptr,   // stored form is not the canonical layout
    /*decode=*/nullptr, // interp-only matcher: no decode-then-scan path (that is
                        // the `fsst` candidate). Compressed-domain match only.
    /*destroy=*/cand_destroy,
};

}  // namespace

// Default visibility so the CMake localize step (which hides every other FSST /
// FSST-LIKE symbol) can still export this single entry point.
extern "C" __attribute__((visibility("default"))) const lb_candidate*
lb_candidate_fsst_like_tum(void) {
  return &kVtable;
}
