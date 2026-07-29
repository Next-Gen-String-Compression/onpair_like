// fsst_like_utn — "Comet" LIKE-push-down for FSST (utndatasystems/fsst-like,
// vendored as a submodule under vendor/fsst-like; DESIGN.md §17): substring
// (LIKE '%needle%') predicates evaluated directly on FSST-compressed bytes with
// a KMP automaton whose transitions are precomputed per FSST symbol — one table
// lookup per compressed code, no decompression.
//
// Like `fsst_like_tum`, this is a compressed-domain MATCHER, not a codec: it
// deliberately exposes NO decode() (that decode-then-scan baseline is the `fsst`
// candidate's job) and answers queries one way — the "comet" strategy.
//
// FSST REUSE: build() and footprint() are the SHARED fsst_common front-end
// (candidates/common/fsst_common/fsst_build.hpp) — byte-for-byte the same
// train+compress+concatenate+export path as `fsst` and `fsst_like_tum`, so all
// three FSST candidates compress identically and their footprints are directly
// comparable. This file's cand_build() just calls fsst_common::Build() and then
// does the one UTN-specific step: rebuild the repo's FsstDecoder from the
// exported symbol table (fsst_import), which drives the comet automaton. The
// FSST fork here (alexandervanrenen, the repo's own submodule) is DISTINCT from
// the `fsst` (cwida) and `fsst_like_tum` (calin2110) forks; all export identical
// fsst_* symbols, so this candidate's FSST symbols are localized at link (see
// CMakeLists.txt) to avoid a duplicate-symbol clash.
//
// Capability envelope: CONTAINS and MULTI_CONTAINS only — the comet
// compressed-domain path is a substring (KMP) matcher; PREFIX/SUFFIX and
// CONTAINS_ANY are out of scope (the repo answers those with decode-based
// engines). CONTAINS matches the needle as a literal byte string, so a '%' byte
// inside a needle (percent-encoded URLs) is matched literally — no wildcard.
// Known limitation: MULTI_CONTAINS joins needles into "%n0%n1%…%" for the repo's
// MetaStateMachine, whose splitter treats '%' as the segment delimiter and has
// NO escaping; a needle containing a literal '%' is therefore mis-segmented.
// Such needles are rare in multi-contains and the correctness gate is the
// backstop.

#include "lb_candidate.h"

// FsstWrapper.hpp includes the fork's <fsst.h> (guarded); StateMachine /
// MetaStateMachine are header-only. Including them brings in the fork's C API
// that fsst_build.hpp needs, so include fsst_build.hpp LAST.
#include "MetaStateMachine.hpp"
#include "StateMachine.hpp"
#include "FsstWrapper.hpp"
#include "Utility.hpp"

#include "fsst_build.hpp"  // shared FSST front-end; include AFTER the fork's fsst.h

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <span>
#include <string>
#include <string_view>

namespace {

// Shared FSST build result + the one UTN-specific extra: the repo's FsstDecoder,
// deserialized once from the exported symbol table, which the comet automaton
// consults (ExtractFsstTable/GetSymbolTable) during matching.
struct Handle {
  fsst_common::FsstBuilt b;
  FsstDecoder decoder;
};

void* cand_build(const lb_chunk_view* view, const char* /*config_json*/,
                 char* err_buf, uint64_t err_cap) {
  auto fail = [&](const char* msg) -> void* {
    if (err_cap > 0) std::snprintf(err_buf, err_cap, "%s", msg);
    return nullptr;
  };

  auto h = std::make_unique<Handle>();
  // Shared front-end: train + compress + concatenate + export.
  if (!fsst_common::Build(view, h->b, err_buf, err_cap)) return nullptr;

  // The comet matcher works entirely off the exported symbol table; the encoder
  // is no longer needed.
  fsst_destroy(h->b.enc);
  h->b.enc = nullptr;

  try {
    // Rebuild the repo's FsstDecoder from the fsst_export() blob (fsst_import).
    // Same fork on both sides of export/import, so the blob round-trips.
    h->decoder.DeserializeDecoder(std::span<const char>(
        reinterpret_cast<const char*>(h->b.symtab.data()), h->b.symtab.size()));
    return h.release();
  } catch (const std::exception& e) {
    return fail((std::string("build failed: ") + e.what()).c_str());
  } catch (...) {
    return fail("build failed: unknown exception");
  }
}

uint32_t cand_footprint(void* self, lb_footprint_component* out,
                        uint32_t capacity) {
  return fsst_common::Footprint(static_cast<Handle*>(self)->b, out, capacity);
}

// "comet" strategy: build the KMP-over-symbols automaton for this query
// (per-query setup, self-timed into setup_ns like a scanner prepare()), then
// drive it over each compressed row. No memoization across calls
// (SEMANTICS.md rule 1).
int cand_run(void* self, uint32_t strategy_index, const lb_query* query,
             uint64_t* out_bitmap_words, lb_run_stats* stats_or_null) {
  auto* h = static_cast<Handle*>(self);
  if (strategy_index != 0) return 10;

  auto set_bit = [out_bitmap_words](uint64_t row) {
    out_bitmap_words[row >> 6] |= uint64_t(1) << (row & 63);
  };
  auto set_all = [&] {
    for (uint64_t i = 0; i < h->b.num_rows; i++) set_bit(i);
  };
  using Clock = std::chrono::steady_clock;
  const auto setup_start = stats_or_null ? Clock::now() : Clock::time_point{};
  auto record_setup = [&] {
    if (stats_or_null) {
      stats_or_null->setup_ns = uint64_t(
          std::chrono::duration_cast<std::chrono::nanoseconds>(Clock::now() -
                                                               setup_start)
              .count());
    }
  };

  // Match compressed row i against a prepared (Meta)StateMachine.
  auto scan_rows = [&](auto& machine) {
    for (uint64_t i = 0; i < h->b.num_rows; i++) {
      const unsigned char* p = h->b.compressed.data() + h->b.coffsets[i];
      const size_t len = size_t(h->b.coffsets[i + 1] - h->b.coffsets[i]);
      if (machine.fsst_lookup_kmp_match(h->decoder, len, p,
                                        h->decoder.GetIdealBufferSize(len)))
        set_bit(i);
    }
  };

  try {
    if (query->op == LB_CONTAINS) {
      const lb_bytes nd = query->needles[0];
      // Empty needle => LIKE '%%' matches every row (and KMP with m==0 is
      // undefined, so short-circuit).
      if (nd.len == 0) {
        record_setup();
        set_all();
        return 0;
      }
      StateMachine machine(
          std::string(reinterpret_cast<const char*>(nd.ptr), nd.len));
      machine.init(h->decoder);
      machine.precompute();
      record_setup();
      scan_rows(machine);
      return 0;
    }

    if (query->op == LB_MULTI_CONTAINS) {
      // "%n0%n1%…%" for the repo's MetaStateMachine/SplitPattern (see the '%'
      // limitation in the file header).
      std::string pat;
      for (uint32_t k = 0; k < query->needle_count; k++) {
        const lb_bytes nd = query->needles[k];
        pat.push_back('%');
        pat.append(reinterpret_cast<const char*>(nd.ptr), nd.len);
      }
      pat.push_back('%');
      MetaStateMachine machine{std::string_view(pat)};
      // No non-empty segment (all needles empty) => matches every row; the meta
      // machine has zero sub-machines and must not be driven.
      if (SplitPattern(pat).empty()) {
        record_setup();
        set_all();
        return 0;
      }
      machine.init(h->decoder);
      machine.precompute();
      record_setup();
      scan_rows(machine);
      return 0;
    }

    return 11;  // unsupported op (PREFIX/SUFFIX/CONTAINS_ANY)
  } catch (...) {
    return 12;
  }
}

void cand_destroy(void* self) { delete static_cast<Handle*>(self); }

const lb_strategy kStrategies[] = {
    {"comet", LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_MULTI_CONTAINS)},
};

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"fsst_like_utn",
    /*version=*/"0.1.0+08a6fd4",
    /*cpu_features=*/nullptr,
    /*strategies=*/kStrategies,
    /*strategy_count=*/1,
    /*build=*/cand_build,
    /*footprint=*/cand_footprint,
    /*run=*/cand_run,
    /*view=*/nullptr,   // stored form is not the canonical layout
    /*decode=*/nullptr, // compressed-domain matcher only (see the `fsst`
                        // candidate for decode-then-scan)
    /*destroy=*/cand_destroy,
};

}  // namespace

// Default visibility so the CMake localize step (which hides every other FSST /
// UTN symbol) can still export this single entry point.
extern "C" __attribute__((visibility("default"))) const lb_candidate*
lb_candidate_fsst_like_utn(void) {
  return &kVtable;
}
