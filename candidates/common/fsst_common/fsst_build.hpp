#pragma once

// fsst_build.hpp — the shared FSST front-end for the `fsst` and `fsst_like_tum`
// candidates. Both train an FSST symbol table and batch-compress the chunk into
// one concatenated code stream; only what they KEEP afterward differs
// (`fsst` decodes over the canonical offsets; `fsst_like_tum` matches in place over
// the compressed-row offsets plus a LIKE automaton built from the trained
// table). This header factors out the identical train+compress+concatenate+
// export core (`Build`) and the common stored-footprint components. Build
// transiently creates both row indexes; every consumer must release the index
// it does not execute against before returning its handle.
//
// IMPORTANT: include your fork's FSST header (cwida `fsst.h`, or calin
// `<fsst/fsst.h>`) BEFORE this one — this header uses the `fsst_*` C API and
// `fsst_encoder_t`/`fsst_decoder_t` without pulling in any specific fork. Its
// functions have INTERNAL linkage (`static`), so each candidate compiles its own
// copy against its own statically-linked, symbol-localized fork; nothing here
// crosses an archive boundary.

#include "lb_candidate.h"

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

namespace fsst_common {

// The result of training + batch-compressing one chunk. Build produces BOTH
// offset arrays so every candidate can share this front-end:
//   - coffsets : compressed-row boundaries into `compressed` (fsst_like_tum/interp)
//   - offsets  : canonical decoded-row boundaries              (fsst/decode)
// They are build intermediates, not a license to retain two indexes: each
// caller releases one (or narrows one and releases both). `enc` is left ALIVE
// so a caller that needs the trained table can use it; every caller destroys it
// once done.
struct FsstBuilt {
  std::vector<uint8_t>  compressed;    // concatenated compressed rows
  std::vector<uint64_t> coffsets;      // num_rows + 1 offsets into `compressed`
  std::vector<uint64_t> offsets;       // num_rows + 1 canonical row offsets
  std::vector<uint8_t>  symtab;        // fsst_export() bytes (clean/pre-clobber)
  fsst_encoder_t*       enc = nullptr; // trained encoder, still alive
  uint64_t num_rows = 0;
  uint64_t payload_bytes = 0;          // canonical payload = offsets[num_rows]
};

// Train + batch-compress `view` into `out`. Returns true on success (with
// out.enc alive); on failure writes a message into err_buf and returns false.
static bool Build(const lb_chunk_view* view, FsstBuilt& out, char* err_buf,
                  uint64_t err_cap) {
  auto fail = [&](const char* msg) -> bool {
    if (err_cap > 0) std::snprintf(err_buf, err_cap, "%s", msg);
    return false;
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

  // Conservative output bound: FSST needs ~(7 + 2*len) per string worst case.
  std::vector<uint8_t> buf;
  std::vector<size_t> len_out(n);
  std::vector<unsigned char*> str_out(n);
  size_t outsize = size_t(7 * n + 2 * payload + 64);
  size_t ncomp = 0;
  for (int attempt = 0; attempt < 8; attempt++) {
    buf.resize(outsize);
    ncomp = fsst_compress(enc, size_t(n), len_in.data(), str_in.data(), outsize,
                          buf.data(), len_out.data(), str_out.data());
    if (ncomp == n) break;
    outsize *= 2;  // output buffer too small for the whole batch — grow & retry
  }
  if (ncomp != n) {
    fsst_destroy(enc);
    return fail("fsst_compress could not fit all rows in the output buffer");
  }

  out.num_rows = n;
  out.payload_bytes = payload;
  // Canonical decoded-row offsets (the chunk's own offsets).
  out.offsets.assign(view->offsets, view->offsets + n + 1);
  // Compressed-row offsets, and the concatenated compressed stream.
  out.coffsets.resize(n + 1);
  out.coffsets[0] = 0;
  for (uint64_t i = 0; i < n; i++) out.coffsets[i + 1] = out.coffsets[i] + len_out[i];
  out.compressed.resize(out.coffsets[n]);
  for (uint64_t i = 0; i < n; i++)
    std::memcpy(out.compressed.data() + out.coffsets[i], str_out[i], len_out[i]);

  // Serialize the (clean, pre-clobber) symbol table.
  out.symtab.resize(sizeof(fsst_decoder_t));
  const unsigned int k = fsst_export(enc, out.symtab.data());
  out.symtab.resize(k);

  out.enc = enc;  // left alive for the caller
  return true;
}

// Force capacity release rather than relying on shrink_to_fit's non-binding
// request. These helpers make the one-index resident policy explicit at every
// shared-builder call site.
static void ReleaseCompressedOffsets(FsstBuilt& b) {
  std::vector<uint64_t>().swap(b.coffsets);
}

static void ReleaseDecodedOffsets(FsstBuilt& b) {
  std::vector<uint64_t>().swap(b.offsets);
}

// The three common footprint components every FSST-family candidate reports.
// `offsets` charges exactly one (num_rows+1)*u64 row index: decoded offsets for
// bulk decode, compressed offsets for match-in-place. A kernel may physically
// narrow its sole index to u32, but the benchmark's current cross-candidate
// policy continues to charge u64 uniformly (DESIGN.md §17.2).
static uint32_t Footprint(const FsstBuilt& b, lb_footprint_component* out,
                          uint32_t capacity) {
  const lb_footprint_component components[] = {
      {"payload_fsst", b.compressed.size()},
      {"symbol_table", b.symtab.size()},
      {"offsets", (b.num_rows + 1) * sizeof(uint64_t)},
  };
  const uint32_t count = 3;
  for (uint32_t i = 0; i < count && i < capacity; i++) out[i] = components[i];
  return count;
}

}  // namespace fsst_common
