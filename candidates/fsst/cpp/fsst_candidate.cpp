// fsst — standard FSST (Fast Static Symbol Table; cwida/fsst upstream, pinned
// in CMakeLists.txt), decode-only. The reference-standard decompress-then-eval
// baseline (DESIGN.md §17): build() trains a symbol table over the chunk and
// compresses every row; the harness composes every scanner over decode()'s
// output (the "decode" strategy). No compressed-domain run() — that is
// fsst_like_tum's job.
//
// build() and footprint() are the SHARED fsst_common front-end
// (candidates/common/fsst_common/fsst_build.hpp), so this candidate and
// fsst_like_tum train/compress/report identically; this file adds only decode +
// the vtable. The shared build also produces compressed-row offsets (which this
// decode-only candidate does not use) so that fsst_like_tum can be served from
// the exact same
// build — see fsst_build.hpp.
//
// Stored form (footprint components, mirroring lz4/zstd/onpair):
//   - payload_fsst : concatenated compressed rows (the payload-analog)
//   - symbol_table : fsst_export() serialization (<= ~2 KB)
//   - offsets      : canonical row index, (rows+1) x u64, uncompressed
//
// FSST code streams are concatenable: decoding the concatenation is equivalent
// to decoding every row and concatenating the outputs. decode() therefore uses
// one bulk fsst_decompress call instead of paying one call/loop boundary for
// every short row. The retained canonical offsets are copied to the output.
// FSST's fixed-stride over-copy is covered by LB_DECODE_PAD (SEMANTICS.md §8).

#include "lb_candidate.h"

#include "fsst.h"

#include "fsst_build.hpp"  // shared front-end; include AFTER fsst.h

#include <cstdint>
#include <cstring>
#include <memory>

namespace {

using fsst_common::FsstBuilt;

void* cand_build(const lb_chunk_view* view, const char* /*config_json*/,
                 char* err_buf, uint64_t err_cap) {
  auto h = std::make_unique<FsstBuilt>();
  if (!fsst_common::Build(view, *h, err_buf, err_cap)) return nullptr;
  // Decode-only: the trained encoder is not needed past build.
  fsst_destroy(h->enc);
  h->enc = nullptr;
  return h.release();
}

uint32_t cand_footprint(void* self, lb_footprint_component* out,
                        uint32_t capacity) {
  return fsst_common::Footprint(*static_cast<FsstBuilt*>(self), out, capacity);
}

int cand_decode(void* self, uint8_t* bytes_out, uint64_t bytes_cap,
                uint64_t* offsets_out) {
  auto* h = static_cast<FsstBuilt*>(self);
  // FSST's inlined decompressor over-copies up to 7 bytes past each row; the
  // contract guarantees bytes_cap >= payload + LB_DECODE_PAD to cover the tail.
  if (bytes_cap < h->payload_bytes) return 1;

  fsst_decoder_t decoder;
  if (fsst_import(&decoder, h->symtab.data()) == 0) return 2;

  const size_t got = fsst_decompress(&decoder, h->compressed.size(),
                                     h->compressed.data(), bytes_cap, bytes_out);
  if (got != h->payload_bytes) return 3;
  std::memcpy(offsets_out, h->offsets.data(),
              h->offsets.size() * sizeof(uint64_t));
  return 0;
}

void cand_destroy(void* self) { delete static_cast<FsstBuilt*>(self); }

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"fsst",
    /*version=*/"0.2.0+e638d4c.bulk",
    /*cpu_features=*/nullptr,
    /*strategies=*/nullptr,
    /*strategy_count=*/0,
    /*build=*/cand_build,
    /*footprint=*/cand_footprint,
    /*run=*/nullptr,
    /*view=*/nullptr,
    /*decode=*/cand_decode,
    /*destroy=*/cand_destroy,
};

}  // namespace

extern "C" const lb_candidate* lb_candidate_fsst(void) { return &kVtable; }
