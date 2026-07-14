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
//   - offsets      : canonical row index, (rows+1) x u64, uncompressed
//
// FSST code streams are concatenable: decoding the concatenation is equivalent
// to decoding every row and concatenating the outputs. decode() therefore uses
// one bulk fsst_decompress call instead of paying one call/loop boundary for
// every short row. The retained canonical offsets are copied to the output.
// FSST's fixed-stride over-copy is covered by LB_DECODE_PAD (SEMANTICS.md §8).

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
  std::vector<uint64_t> offsets;     // num_rows + 1 canonical row offsets
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
  h->offsets.assign(view->offsets, view->offsets + n + 1);

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

  // fsst_compress lays rows contiguously in `out`; FSST code streams can be
  // concatenated and decoded as one stream, so no compressed-row index is
  // needed by this decode-only candidate.
  uint64_t total = 0;
  for (uint64_t i = 0; i < n; i++) total += len_out[i];
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
      {"offsets", h->offsets.size() * sizeof(uint64_t)},
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

  const size_t got = fsst_decompress(&decoder, h->compressed.size(),
                                     h->compressed.data(), bytes_cap, bytes_out);
  if (got != h->payload_bytes) return 3;
  std::memcpy(offsets_out, h->offsets.data(),
              h->offsets.size() * sizeof(uint64_t));
  return 0;
}

void cand_destroy(void* self) { delete static_cast<Handle*>(self); }

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
