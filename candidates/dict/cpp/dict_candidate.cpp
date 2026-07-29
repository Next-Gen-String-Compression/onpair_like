// dict — plain dictionary encoding, decode-only. A decompress-then-eval
// baseline in the lz4/zstd/fsst shape: build() deduplicates the chunk into a
// FlatDictionary (one contiguous blob of unique strings + one code per row);
// the harness composes every scanner over decode()'s output (the "decode"
// strategy). No compressed-domain run()/view() — a dictionary column could
// evaluate a predicate once per unique entry, but this candidate mirrors the
// reference (CompressionBenchmark's DictionaryAlgorithm), which is decode-only.
//
// Stored form (footprint components). Codes and lengths are reported at their
// bit-packed size (bitpacking_size.hpp), the representative resident cost of a
// dictionary column — a code needs ceil(log2(n_unique+1)) bits, not a whole
// u32 — matching the reference's CompressedSizeInfo::Dictionary. The in-memory
// arrays stay full-width; decode() reads them directly, so only the report
// reflects packing (see bitpacking_size.hpp).
//   - dict_blob    : the unique strings, packed contiguously (the payload-analog)
//   - dict_lengths : bit-packed unique-string lengths (locate the blob; offsets
//                    are the prefix sum, not stored)
//   - codes        : bit-packed per-row dictionary codes (the per-row data)
// Canonical row offsets are reconstructed at decode time as the prefix sum of
// the decoded row lengths, so they are neither stored nor counted.
//
// FlatDictionary::Copy16 over-copies up to 15 bytes per row; the blob is padded
// by COPY_CHUNK and the output buffer's LB_DECODE_PAD covers the tail.

#include "lb_candidate.h"

#include "flat_dictionary.hpp"
#include "bitpacking_size.hpp"

#include <cstdint>
#include <cstdio>
#include <memory>

namespace {

struct Handle {
  FlatDictionary dict;
  uint64_t num_rows = 0;
  uint64_t payload_bytes = 0;   // canonical payload = offsets[num_rows]
};

void* cand_build(const lb_chunk_view* view, const char* /*config_json*/,
                 char* /*err_buf*/, uint64_t /*err_cap*/) {
  const uint64_t n = view->num_rows;
  auto h = std::make_unique<Handle>();
  h->num_rows = n;
  h->payload_bytes = view->offsets[n];
  h->dict.Build(view->bytes, view->offsets, n);
  return h.release();
}

uint32_t cand_footprint(void* self, lb_footprint_component* out,
                        uint32_t capacity) {
  auto* h = static_cast<Handle*>(self);
  const lb_footprint_component components[] = {
      {"dict_blob", h->dict.BlobSize()},
      {"dict_lengths", bitpack::CompressedSize(h->dict.Lengths())},
      {"codes", bitpack::CompressedSize(h->dict.NumUnique(), h->dict.NumRows())},
  };
  const uint32_t count = 3;
  for (uint32_t i = 0; i < count && i < capacity; i++) out[i] = components[i];
  return count;
}

int cand_decode(void* self, uint8_t* bytes_out, uint64_t bytes_cap,
                uint64_t* offsets_out) {
  auto* h = static_cast<Handle*>(self);
  // Copy16 over-copies up to 15 bytes past the last row; the contract
  // guarantees bytes_cap >= payload + LB_DECODE_PAD to cover the tail.
  if (bytes_cap < h->payload_bytes) return 1;

  const uint8_t* blob = h->dict.Blob();
  const std::vector<uint32_t>& offs = h->dict.Offsets();
  const std::vector<uint32_t>& lens = h->dict.Lengths();
  const std::vector<uint32_t>& codes = h->dict.Codes();

  uint8_t* write_ptr = bytes_out;
  uint64_t acc = 0;
  offsets_out[0] = 0;
  for (uint64_t i = 0; i < h->num_rows; i++) {
    const uint32_t code = codes[i];
    const uint32_t len = lens[code];
    FlatDictionary::Copy16(write_ptr, blob + offs[code], len);
    write_ptr += len;
    acc += len;
    offsets_out[i + 1] = acc;
  }
  return 0;
}

void cand_destroy(void* self) { delete static_cast<Handle*>(self); }

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"dict",
    /*version=*/"0.1.0",
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

extern "C" const lb_candidate* lb_candidate_dict(void) { return &kVtable; }
