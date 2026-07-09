// fsst — standard FSST (Fast Static Symbol Table; cwida/fsst upstream, pinned
// in CMakeLists.txt), decode-only. The reference-standard decompress-then-eval
// baseline (DESIGN.md §17): build() trains a symbol table over the chunk and
// compresses every row; the harness composes every scanner over decode()'s
// output (the "decode" strategy). No compressed-domain run() — that is
// fsst_like's job.
//
// Stored form (footprint components, mirroring lz4/zstd/onpair):
//   - payload_fsst : concatenated compressed rows (the payload-analog)
//   - symbol_table : fsst_export() serialization (<= ~2 KB)
//   - offsets      : compressed-row index, (rows+1) x u64, uncompressed
//
// decode() reconstructs the canonical layout: FSST's inlined decompressor does
// fixed-stride 8-byte over-copies per symbol (writing up to 7 bytes past a
// row's true end), which the contract's LB_DECODE_PAD headroom absorbs — the
// same guarantee onpair relies on (SEMANTICS.md rule 8).

#include "lb_candidate.h"

#include "fsst.h"

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <memory>
#include <new>
#include <vector>

namespace {

struct Handle {
  std::vector<uint8_t> compressed;   // concatenated compressed rows
  std::vector<uint64_t> coffsets;    // num_rows + 1 offsets into `compressed`
  std::vector<uint8_t> symtab;       // fsst_export() bytes
  uint64_t num_rows = 0;
  uint64_t payload_bytes = 0;        // canonical payload = offsets[num_rows]
};

void* cand_build(const lb_chunk_view* view, const char* /*config_json*/,
                 char* err_buf, uint64_t err_cap) {
  auto fail = [&](const char* msg) -> void* {
    if (err_cap > 0) std::snprintf(err_buf, err_cap, "%s", msg);
    return nullptr;
  };
  const uint64_t n = view->num_rows;
  const uint64_t payload = view->offsets[n];

  // Per-row (len, ptr) views into the chunk for the FSST batch API.
  std::vector<size_t> len_in(n);
  std::vector<const unsigned char*> str_in(n);
  for (uint64_t i = 0; i < n; i++) {
    len_in[i] = size_t(view->offsets[i + 1] - view->offsets[i]);
    str_in[i] = view->bytes + view->offsets[i];
  }

  fsst_encoder_t* enc =
      fsst_create(size_t(n), len_in.data(), str_in.data(), /*zeroTerminated=*/0);
  if (enc == nullptr) return fail("fsst_create failed");

  auto h = std::make_unique<Handle>();
  h->num_rows = n;
  h->payload_bytes = payload;

  // Conservative output bound: FSST needs ~(7 + 2*len) per string worst case.
  std::vector<uint8_t> out;
  std::vector<size_t> len_out(n);
  std::vector<unsigned char*> str_out(n);
  size_t outsize = size_t(7 * n + 2 * payload + 64);
  size_t ncomp = 0;
  for (int attempt = 0; attempt < 8; attempt++) {
    out.resize(outsize);
    ncomp = fsst_compress(enc, size_t(n), len_in.data(), str_in.data(), outsize,
                          out.data(), len_out.data(), str_out.data());
    if (ncomp == n) break;
    outsize *= 2;  // output buffer too small for the whole batch — grow & retry
  }
  if (ncomp != n) {
    fsst_destroy(enc);
    return fail("fsst_compress could not fit all rows in the output buffer");
  }

  // Prefix-sum the per-row compressed lengths into the compressed-row index.
  // fsst_compress lays the compressed rows out contiguously in `out`, so the
  // concatenation is out[0 .. total); we build coffsets from len_out (robust
  // regardless of that contiguity guarantee) and copy the payload.
  h->coffsets.resize(n + 1);
  h->coffsets[0] = 0;
  for (uint64_t i = 0; i < n; i++) h->coffsets[i + 1] = h->coffsets[i] + len_out[i];
  const uint64_t total = h->coffsets[n];
  h->compressed.assign(out.data(), out.data() + total);

  // Serialize the symbol table (at most sizeof(fsst_decoder_t) bytes).
  h->symtab.resize(sizeof(fsst_decoder_t));
  const unsigned int k = fsst_export(enc, h->symtab.data());
  h->symtab.resize(k);

  fsst_destroy(enc);
  return h.release();
}

uint32_t cand_footprint(void* self, lb_footprint_component* out,
                        uint32_t capacity) {
  auto* h = static_cast<Handle*>(self);
  const lb_footprint_component components[] = {
      {"payload_fsst", h->compressed.size()},
      {"symbol_table", h->symtab.size()},
      {"offsets", h->coffsets.size() * sizeof(uint64_t)},
  };
  const uint32_t count = 3;
  for (uint32_t i = 0; i < count && i < capacity; i++) out[i] = components[i];
  return count;
}

int cand_decode(void* self, uint8_t* bytes_out, uint64_t bytes_cap,
                uint64_t* offsets_out) {
  auto* h = static_cast<Handle*>(self);
  // FSST's inlined decompressor over-copies up to 7 bytes past each row; the
  // contract guarantees bytes_cap >= payload + LB_DECODE_PAD to cover the tail.
  if (bytes_cap < h->payload_bytes) return 1;

  fsst_decoder_t decoder;
  if (fsst_import(&decoder, h->symtab.data()) == 0) return 2;

  uint64_t pos = 0;
  for (uint64_t i = 0; i < h->num_rows; i++) {
    offsets_out[i] = pos;
    const size_t clen = size_t(h->coffsets[i + 1] - h->coffsets[i]);
    const unsigned char* cptr = h->compressed.data() + h->coffsets[i];
    const size_t got = fsst_decompress(&decoder, clen, cptr, bytes_cap - pos,
                                       bytes_out + pos);
    pos += got;
  }
  offsets_out[h->num_rows] = pos;
  if (pos != h->payload_bytes) return 3;
  return 0;
}

void cand_destroy(void* self) { delete static_cast<Handle*>(self); }

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"fsst",
    /*version=*/"0.1.0+e638d4c",
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
