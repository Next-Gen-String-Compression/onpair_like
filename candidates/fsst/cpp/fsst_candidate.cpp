// fsst — standard FSST (Fast Static Symbol Table; cwida/fsst upstream, pinned
// in CMakeLists.txt), decode-only. The reference-standard decompress-then-eval
// baseline (DESIGN.md §17): build() trains a symbol table over the chunk and
// compresses every row; the harness composes every scanner over decode()'s
// output (the "decode" strategy). No compressed-domain run() — that is
// fsst_like_tum's job.
//
// build() uses the SHARED fsst_common front-end
// (candidates/common/fsst_common/fsst_build.hpp), so this candidate and
// fsst_like_tum train and compress identically. This decode-only wrapper drops
// the shared build's compressed-row offsets, retains a prepared decoder, and
// reports that decoder in addition to the common stored components.
//
// Stored form (footprint components, mirroring lz4/zstd/onpair):
//   - payload_fsst : concatenated compressed rows (the payload-analog)
//   - symbol_table : fsst_export() serialization (<= ~2 KB)
//   - offsets      : canonical row index, (rows+1) x u64, uncompressed
//   - decode_table : retained fsst_decoder_t used by bulk decompression
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

struct FsstDecodeBuilt {
  FsstBuilt storage;
  // Imported once at build. It is read-only during decompression and costs
  // about 2 KiB; rebuilding it in every query's decode phase both adds noise
  // and violates the steady-state codec model.
  fsst_decoder_t decoder{};
};

void* cand_build(const lb_chunk_view* view, const char* /*config_json*/,
                 char* err_buf, uint64_t err_cap) {
  auto h = std::make_unique<FsstDecodeBuilt>();
  if (!fsst_common::Build(view, h->storage, err_buf, err_cap)) return nullptr;
  // The shared builder needs compressed-row offsets while concatenating its
  // per-row outputs. Bulk decode consumes the resulting stream as one unit and
  // needs only canonical decoded offsets, so release the unused second index.
  fsst_common::ReleaseCompressedOffsets(h->storage);
  // Decode-only: the trained encoder is not needed past build.
  fsst_destroy(h->storage.enc);
  h->storage.enc = nullptr;
  if (fsst_import(&h->decoder, h->storage.symtab.data()) == 0) {
    if (err_cap > 0) std::snprintf(err_buf, err_cap, "%s", "fsst_import failed");
    return nullptr;
  }
  return h.release();
}

uint32_t cand_footprint(void* self, lb_footprint_component* out,
                        uint32_t capacity) {
  auto* h = static_cast<FsstDecodeBuilt*>(self);
  const uint32_t count = fsst_common::Footprint(h->storage, out, capacity);
  if (capacity > count) {
    out[count] = {"decode_table", sizeof(fsst_decoder_t)};
  }
  return count + 1;
}

int cand_decode(void* self, uint8_t* bytes_out, uint64_t bytes_cap,
                uint64_t* offsets_out) {
  auto* h = static_cast<FsstDecodeBuilt*>(self);
  auto& storage = h->storage;
  // FSST's inlined decompressor over-copies up to 7 bytes past each row; the
  // contract guarantees bytes_cap >= payload + LB_DECODE_PAD to cover the tail.
  if (bytes_cap < storage.payload_bytes) return 1;

  const size_t got =
      fsst_decompress(&h->decoder, storage.compressed.size(),
                      storage.compressed.data(), bytes_cap, bytes_out);
  if (got != storage.payload_bytes) return 3;
  std::memcpy(offsets_out, storage.offsets.data(),
              storage.offsets.size() * sizeof(uint64_t));
  return 0;
}

void cand_destroy(void* self) { delete static_cast<FsstDecodeBuilt*>(self); }

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"fsst",
    /*version=*/"0.3.0+e638d4c.bulk.decoder-once",
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
