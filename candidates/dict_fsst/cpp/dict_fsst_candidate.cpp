// dict_fsst — dictionary encoding whose unique strings are FSST-compressed,
// decode-only. Built on FlatDictionary for the dedup + per-row codes (like the
// `dict` candidate); the unique strings are then FSST-compressed as their own
// batch. Decode FSST-decompresses the whole (concatenated) unique set once into
// a flat buffer, then reconstructs each row with a memcpy — the same
// whole-dictionary strategy as the reference (CompressionBenchmark's
// DictionaryFsstAlgorithm), the honest analog of a full column scan. No
// compressed-domain run()/view(); the harness composes every scanner over
// decode()'s output.
//
// The FSST front-end is the SHARED fsst_common::Build (common/fsst_common):
// this candidate simply hands it a chunk view over the *unique* strings, so it
// trains/compresses/concatenates/exports exactly like the `fsst` candidate — no
// duplicated FSST boilerplate. Because FSST code streams concatenate, decode is
// a single bulk fsst_decompress of the whole unique set. Wraps cwida/fsst
// upstream (pinned in CMakeLists.txt), same fork as the `fsst` candidate.
//
// Stored form (footprint components). As in `dict`, codes and per-entry lengths
// are reported at their bit-packed size (bitpacking_size.hpp), matching the
// reference's CompressedSizeInfo::DictionaryFSST. In-memory arrays stay
// full-width; decode reads them directly, so only the report reflects packing.
//   - dict_fsst_payload : Σ compressed unique-entry byte-lengths (payload-analog)
//   - symbol_table      : fsst_export() serialization (<= ~2 KB)
//   - dict_clengths     : bit-packed compressed-entry lengths (locate each entry
//                         in the compressed blob)
//   - codes             : bit-packed per-row dictionary codes

#include "lb_candidate.h"

#include "fsst.h"

#include "fsst_build.hpp"       // shared FSST front-end; include AFTER fsst.h
#include "flat_dictionary.hpp"
#include "bitpacking_size.hpp"

#include <cstdint>
#include <cstdio>
#include <memory>
#include <vector>

namespace {

struct Handle {
  // FSST-compressed *unique* entries: b.num_rows == n_unique, b.offsets are the
  // unique-string boundaries in the decoded blob, b.compressed/b.coffsets the
  // compressed entries. (b.enc is destroyed at end of build.)
  fsst_common::FsstBuilt b;
  std::vector<uint32_t> codes;  // one dictionary code per row
  uint64_t num_rows = 0;        // total rows
  uint64_t payload_bytes = 0;   // total canonical payload
};

void* cand_build(const lb_chunk_view* view, const char* /*config_json*/,
                 char* err_buf, uint64_t err_cap) {
  const uint64_t n = view->num_rows;

  FlatDictionary dict;
  dict.Build(view->bytes, view->offsets, n);
  const size_t n_unique = dict.NumUnique();

  auto h = std::make_unique<Handle>();
  h->num_rows = n;
  h->payload_bytes = view->offsets[n];
  h->codes.assign(dict.Codes().begin(), dict.Codes().end());
  if (n_unique == 0) return h.release();  // empty chunk

  // Present the unique strings as a chunk view (u64 offsets over the dictionary
  // blob), then FSST-compress them via the shared front-end. FlatDictionary lays
  // uniques contiguously, so offset_[i+1] == offset_[i] + length_[i]; the final
  // offset is the blob size.
  const std::vector<uint32_t>& offs = dict.Offsets();
  std::vector<uint64_t> uoff(n_unique + 1);
  for (size_t i = 0; i < n_unique; i++) uoff[i] = offs[i];
  uoff[n_unique] = dict.BlobSize();
  const lb_chunk_view unique_view{dict.Blob(), uoff.data(), n_unique};

  if (!fsst_common::Build(&unique_view, h->b, err_buf, err_cap)) return nullptr;
  fsst_destroy(h->b.enc);  // decode-only: the trained encoder is not needed
  h->b.enc = nullptr;
  return h.release();
}

uint32_t cand_footprint(void* self, lb_footprint_component* out,
                        uint32_t capacity) {
  auto* h = static_cast<Handle*>(self);
  const size_t n_unique = h->b.num_rows;  // b indexes unique entries

  // Compressed-entry lengths (from the compressed-row offsets) for the
  // bit-packed size, matching the reference's dictionary_lengths accounting.
  std::vector<uint32_t> clengths(n_unique);
  for (size_t i = 0; i < n_unique; i++)
    clengths[i] = static_cast<uint32_t>(h->b.coffsets[i + 1] - h->b.coffsets[i]);

  const lb_footprint_component components[] = {
      {"dict_fsst_payload", h->b.compressed.size()},
      {"symbol_table", h->b.symtab.size()},
      {"dict_clengths", bitpack::CompressedSize(clengths)},
      {"codes", bitpack::CompressedSize(n_unique, h->num_rows)},
  };
  const uint32_t count = 4;
  for (uint32_t i = 0; i < count && i < capacity; i++) out[i] = components[i];
  return count;
}

int cand_decode(void* self, uint8_t* bytes_out, uint64_t bytes_cap,
                uint64_t* offsets_out) {
  auto* h = static_cast<Handle*>(self);
  if (bytes_cap < h->payload_bytes) return 1;

  offsets_out[0] = 0;
  const size_t n_unique = h->b.num_rows;
  if (n_unique == 0) return 0;  // empty chunk

  fsst_decoder_t decoder;
  if (fsst_import(&decoder, h->b.symtab.data()) == 0) return 2;

  // Concatenated FSST streams decode as one: a single bulk decompress rebuilds
  // the whole unique blob (padded by COPY_CHUNK for the tail over-copy).
  const uint64_t unique_blob = h->b.offsets[n_unique];
  std::vector<uint8_t> decoded(unique_blob + FlatDictionary::COPY_CHUNK);
  const size_t got = fsst_decompress(&decoder, h->b.compressed.size(),
                                     h->b.compressed.data(), decoded.size(),
                                     decoded.data());
  if (got != unique_blob) return 3;

  // Reconstruct each row: memcpy its unique entry out of the decoded blob.
  const std::vector<uint64_t>& uoff = h->b.offsets;
  uint8_t* write_ptr = bytes_out;
  uint64_t acc = 0;
  for (uint64_t i = 0; i < h->num_rows; i++) {
    const uint32_t code = h->codes[i];
    const uint32_t len = static_cast<uint32_t>(uoff[code + 1] - uoff[code]);
    FlatDictionary::Copy16(write_ptr, decoded.data() + uoff[code], len);
    write_ptr += len;
    acc += len;
    offsets_out[i + 1] = acc;
  }
  return 0;
}

void cand_destroy(void* self) { delete static_cast<Handle*>(self); }

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"dict_fsst",
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

extern "C" const lb_candidate* lb_candidate_dict_fsst(void) { return &kVtable; }
