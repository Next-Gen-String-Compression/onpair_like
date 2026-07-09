// fsst_like — FSST-LIKE-Matching (calin2110/FSST-LIKE-Matching, DaMoN'26;
// pinned in CMakeLists.txt): LIKE/substring predicates evaluated directly on
// FSST-compressed bytes via a per-pattern finite automaton (DESIGN.md §17).
//
// Two ways to answer queries:
//   - candidate strategy "interp": build the interpreted automaton
//     (LikePatternAutomatonParser) for the query, then drive parse() over each
//     row's compressed bytes. (C++-codegen / LLVM-JIT backends land next.)
//   - harness-composed "decode": standard FSST decompress into scratch, then
//     any scanner — the decode-vs-match-in-place comparison on IDENTICAL bytes.
//
// build() mirrors the repo's own compression (fa-drawing/server.cpp): train an
// FSST symbol table, compress every row, and write the escaped-byte bitmap into
// symbols[255] that the matcher's isEscapable() reads. The FSST fork here
// (calin2110) is DISTINCT from the `fsst` candidate's cwida upstream; both
// statically link a full FSST copy, so this candidate's FSST symbols are
// localized at link (see CMakeLists.txt) to avoid a duplicate-symbol clash.
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

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <memory>
#include <span>
#include <string>
#include <vector>

namespace {

struct Handle {
  std::vector<uint8_t> compressed;    // concatenated compressed rows
  std::vector<uint64_t> coffsets;     // num_rows + 1 offsets into `compressed`
  std::vector<uint8_t> symtab_export; // fsst_export() bytes (pre-clobber)
  std::unique_ptr<Encoder> encoder;   // FSST-LIKE Encoder (clobbered table) for the matcher
  uint64_t num_rows = 0;
  uint64_t payload_bytes = 0;
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
  const uint64_t n = view->num_rows;
  const uint64_t payload = view->offsets[n];

  std::vector<size_t> len_in(n);
  std::vector<const unsigned char*> str_in(n);
  for (uint64_t i = 0; i < n; i++) {
    len_in[i] = size_t(view->offsets[i + 1] - view->offsets[i]);
    str_in[i] = view->bytes + view->offsets[i];
  }

  fsst_encoder_t* enc =
      fsst_create(size_t(n), len_in.data(), str_in.data(), /*zeroTerminated=*/0);
  if (enc == nullptr) return fail("fsst_create failed");

  try {
    // The libfsst::SymbolTable this encoder trained. Shared: keeping a copy of
    // the shared_ptr keeps it alive after fsst_destroy(enc).
    std::shared_ptr<libfsst::SymbolTable> sym =
        reinterpret_cast<libfsst::Encoder*>(enc)->symbolTable;

    std::vector<uint8_t> out;
    std::vector<size_t> len_out(n);
    std::vector<unsigned char*> str_out(n);
    size_t outsize = size_t(7 * n + 2 * payload + 64);
    size_t ncomp = 0;
    for (int attempt = 0; attempt < 8; attempt++) {
      out.resize(outsize);
      ncomp = fsst_compress(enc, size_t(n), len_in.data(), str_in.data(),
                            outsize, out.data(), len_out.data(), str_out.data());
      if (ncomp == n) break;
      outsize *= 2;
    }
    if (ncomp != n) {
      fsst_destroy(enc);
      return fail("fsst_compress could not fit all rows in the output buffer");
    }

    auto h = std::make_unique<Handle>();
    h->num_rows = n;
    h->payload_bytes = payload;
    h->coffsets.resize(n + 1);
    h->coffsets[0] = 0;
    for (uint64_t i = 0; i < n; i++) h->coffsets[i + 1] = h->coffsets[i] + len_out[i];
    h->compressed.resize(h->coffsets[n]);
    for (uint64_t i = 0; i < n; i++)
      std::memcpy(h->compressed.data() + h->coffsets[i], str_out[i], len_out[i]);

    // Serialize the symbol table BEFORE clobbering symbols[255] (below), so the
    // decode path gets a clean, standard FSST header.
    h->symtab_export.resize(sizeof(fsst_decoder_t));
    const unsigned int k = fsst_export(enc, h->symtab_export.data());
    h->symtab_export.resize(k);

    // Escaped-byte bitmap into symbols[255], exactly as the repo's compressFile:
    // isEscapable(b) reads reinterpret_cast<bool*>(&symbols[255])[b].
    {
      bool bitmap[256] = {false};
      for (uint64_t i = 0; i < n; i++) {
        size_t j = 0;
        while (j < len_out[i]) {
          if (str_out[i][j] == 255) { ++j; bitmap[str_out[i][j]] = true; }
          ++j;
        }
      }
      std::memcpy(&sym->symbols[255], bitmap, 256 * sizeof(bool));
    }

    // FSST-LIKE Encoder over the (clobbered) table, for automaton construction.
    SymbolTable st(sym);
    h->encoder = std::make_unique<Encoder>(st);

    fsst_destroy(enc);
    return h.release();
  } catch (const std::exception& e) {
    fsst_destroy(enc);
    return fail((std::string("build failed: ") + e.what()).c_str());
  } catch (...) {
    fsst_destroy(enc);
    return fail("build failed: unknown exception");
  }
}

uint32_t cand_footprint(void* self, lb_footprint_component* out,
                        uint32_t capacity) {
  auto* h = static_cast<Handle*>(self);
  const lb_footprint_component components[] = {
      {"payload_fsst", h->compressed.size()},
      {"symbol_table", h->symtab_export.size()},
      {"offsets", h->coffsets.size() * sizeof(uint64_t)},
  };
  const uint32_t count = 3;
  for (uint32_t i = 0; i < count && i < capacity; i++) out[i] = components[i];
  return count;
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
    for (uint64_t i = 0; i < h->num_rows; i++) {
      const uint8_t* p = h->compressed.data() + h->coffsets[i];
      const size_t len = size_t(h->coffsets[i + 1] - h->coffsets[i]);
      if (parser.parse(std::span<const uint8_t>(p, len))) set_bit(i);
    }
    return 0;
  } catch (...) {
    return 12;
  }
}

int cand_decode(void* self, uint8_t* bytes_out, uint64_t bytes_cap,
                uint64_t* offsets_out) {
  auto* h = static_cast<Handle*>(self);
  if (bytes_cap < h->payload_bytes) return 1;
  fsst_decoder_t decoder;
  if (fsst_import(&decoder, h->symtab_export.data()) == 0) return 2;
  uint64_t pos = 0;
  for (uint64_t i = 0; i < h->num_rows; i++) {
    offsets_out[i] = pos;
    const size_t clen = size_t(h->coffsets[i + 1] - h->coffsets[i]);
    const unsigned char* cptr = h->compressed.data() + h->coffsets[i];
    pos += fsst_decompress(&decoder, clen, cptr, bytes_cap - pos, bytes_out + pos);
  }
  offsets_out[h->num_rows] = pos;
  return pos == h->payload_bytes ? 0 : 3;
}

void cand_destroy(void* self) { delete static_cast<Handle*>(self); }

const lb_strategy kStrategies[] = {
    {"interp", LB_OP_BIT(LB_PREFIX) | LB_OP_BIT(LB_SUFFIX) |
                   LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_MULTI_CONTAINS)},
};

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"fsst_like",
    /*version=*/"0.1.0+b1eb3ab",
    /*cpu_features=*/nullptr,
    /*strategies=*/kStrategies,
    /*strategy_count=*/1,
    /*build=*/cand_build,
    /*footprint=*/cand_footprint,
    /*run=*/cand_run,
    /*view=*/nullptr,  // stored form is not the canonical layout
    /*decode=*/cand_decode,
    /*destroy=*/cand_destroy,
};

}  // namespace

// Sole exported symbol: everything else (FSST + FSST-LIKE) is localized at link.
extern "C" __attribute__((visibility("default"))) const lb_candidate*
lb_candidate_fsst_like(void) {
  return &kVtable;
}
