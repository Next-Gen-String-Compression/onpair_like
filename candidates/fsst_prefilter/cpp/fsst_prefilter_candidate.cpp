// fsst_prefilter — COMPRESSED-DOMAIN prefilter over FSST, and its dictionary
// peer dict_fsst_prefilter. Two candidates from one FSST copy (this file exports
// both entry points; CMakeLists localizes the rest).
//
// Unlike fsst_decode_prefilter (which decompresses the whole column, then
// prefilters plaintext), this prefilters DIRECTLY on the compressed bytes:
//   1. build()  FSST-trains + compresses the column.
//   2. per query, translate each needle into its mandatory FSST-code chain
//      (fsst_mandatory_chain.hpp) — the run of codes that MUST appear in any
//      matching row's compressed stream — and pack the first 4 (or 2) codes into
//      a 2/4-byte code.
//   3. scan the COMPRESSED stream for those codes with the vendored FLAT
//      VECTORIZED prefilter kernel (see below). No decompression on rejected
//      rows.
//   4. decompress only the SURVIVORS (per row) and verify the real needle.
// This mirrors DuckDB's ContainsPrefilterExecptor FSST path (see
// candidates/common/fsst_common/table_filter_state.cpp): mandatory-chain codes
// -> PrefilterContainsAny on the compressed view -> exact filter downstream.
//
//   dict_fsst_prefilter : the same matcher as a DictMatcher child, so chain
//                         building + the compressed scan run once per UNIQUE value.
//
// THE SCAN: `u{16,32}::flat::simd::contains` from
// candidates/common/prefilter/vendor/fsst_prefilter (linked, not re-included —
// the kernel keeps the codegen it was tuned and disassembled with). `flat` is
// the buffer-at-a-time layout: base + n+1 offsets, rows back to back, pass 1
// sweeping 128-byte superblocks with no idea rows exist and pass 2 mapping the
// hit positions back to rows through the offsets. `simd` is the NEON/SSE2
// spelling, which fuses all K codes into shared block accumulators — the
// marginal cost of a code is its compares, not another pass over the bytes.
// That combination is the fastest cell of the vendor's grid at both widths
// (u32: 5.8 ms vs 15.1 for the row/scalar shape this candidate used before).
//
// Two contracts the flat layout imposes, both satisfied in build():
//   * 32-bit offsets into one contiguous buffer -> coff32_.
//   * BUFFER_SLACK readable bytes past the last row, because a superblock read
//     at the buffer end runs CODE_LEN-1 bytes over -> the padded compressed
//     vector (footprint() reports the unpadded size).
// The flat kernels keep a static position scratch and are NOT thread-safe; the
// harness evaluates one candidate at a time on one thread.
//
// A needle with no usable chain (< 2 mandatory codes for this symbol table)
// degrades to pass-through (every row a survivor); the verify is the correctness
// authority. Second cwida FSST copy -> all FSST symbols localized at link.

#include "lb_candidate.h"

#include "fsst.h"

#include "fsst_build.hpp"             // shared front-end; include AFTER fsst.h
#include "fsst_mandatory_chain.hpp"

#include "matcher.hpp"
#include "dict_matcher.hpp"

#include "contains/kernel.hpp"             // VECTOR_SIZE, BUFFER_SLACK
#include "contains/kernels/geometry.hpp"   // pf::MAX_SYMBOLS
#include "contains/kernels/simd_shim.hpp"  // PREFILTER_HAVE_SIMD

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <memory>
#include <string_view>
#include <vector>

#if !defined(PREFILTER_HAVE_SIMD)
#error "fsst_prefilter needs the vendored simd kernels (NEON on aarch64, SSE2 on x86-64)"
#endif

// The vendored kernels' public entry points, defined in
// vendor/fsst_prefilter/src/contains/kernels/candidates/candidates_u{16,32}.cpp.
// That project exports them by definition only — there is no header — so they
// are declared here; the signatures are fixed by its kernel.hpp contract.
namespace u32::flat::simd {
uint32_t contains(const char* base, const uint32_t* offsets, uint32_t n,
                  const uint32_t* codes, uint32_t k, uint32_t* sel);
}  // namespace u32::flat::simd
namespace u16::flat::simd {
uint32_t contains(const char* base, const uint32_t* offsets, uint32_t n,
                  const uint16_t* codes, uint32_t k, uint32_t* sel);
}  // namespace u16::flat::simd

namespace {

class FsstPrefilter final : public lb::Matcher {
 public:
  bool build(const uint8_t* bytes, const uint64_t* offsets, uint64_t n, char* eb,
             uint64_t ec) override {
    const lb_chunk_view view{bytes, offsets, n};
    if (!fsst_common::Build(&view, b_, eb, ec)) return false;
    if (fsst_import(&decoder_, b_.symtab.data()) == 0) {
      fsst_destroy(b_.enc);
      b_.enc = nullptr;
      if (ec > 0) std::snprintf(eb, ec, "fsst_import failed");
      return false;
    }
    fsst_destroy(b_.enc);
    b_.enc = nullptr;
    // Per-row decode scratch (survivors only): the longest decoded row + pad.
    uint64_t max_row = 0;
    for (uint64_t i = 0; i < n; i++) {
      const uint64_t len = offsets[i + 1] - offsets[i];
      if (len > max_row) max_row = len;
    }
    rowbuf_.resize(max_row + LB_DECODE_PAD);

    // Flat-scan plumbing. The kernels index one contiguous buffer with 32-bit
    // absolute offsets, so the u64 compressed-row offsets are narrowed once
    // here and then released. Footprint still charges the benchmark's uniform
    // u64-per-row index policy; actual execution retains only this u32 index.
    cbytes_ = b_.coffsets[n];
    if (cbytes_ > uint64_t{UINT32_MAX} - BUFFER_SLACK) {
      if (ec > 0)
        std::snprintf(eb, ec, "compressed chunk is %llu B, past the flat kernels' 32-bit offsets",
                      static_cast<unsigned long long>(cbytes_));
      return false;
    }
    coff32_.resize(n + 1);
    for (uint64_t i = 0; i <= n; i++) coff32_[i] = static_cast<uint32_t>(b_.coffsets[i]);
    fsst_common::ReleaseCompressedOffsets(b_);
    fsst_common::ReleaseDecodedOffsets(b_);
    // Readable slack past the last row: a superblock probe starting inside the
    // buffer reads up to CODE_LEN-1 bytes past its end.
    b_.compressed.resize(cbytes_ + BUFFER_SLACK, 0);
    return true;
  }

  int run(const lb_query* q, uint64_t* out, lb_run_stats* stats) override {
    if (q->op != LB_CONTAINS && q->op != LB_CONTAINS_ANY) return 1;

    // Empty needle among the set => '%%' matches every row.
    for (uint32_t k = 0; k < q->needle_count; k++) {
      if (q->needles[k].len == 0) {
        for (uint64_t i = 0; i < b_.num_rows; i++) out[i >> 6] |= uint64_t{1} << (i & 63);
        if (stats) stats->prefilter_candidates = b_.num_rows;
        return 0;
      }
    }

    // Per-needle mandatory-chain code targets. One width for the whole set: the
    // shortest chain picks it (prefer 4 codes, fall back to 2, else pass-through).
    std::vector<uint32_t> t32;
    std::vector<uint16_t> t16;
    size_t min_chain = SIZE_MAX;
    std::vector<std::vector<uint8_t>> chains;
    for (uint32_t k = 0; k < q->needle_count; k++) {
      chains.push_back(fsst_common::FsstMandatoryChain(
          decoder_.len, reinterpret_cast<const uint64_t*>(decoder_.symbol),
          q->needles[k].ptr, static_cast<size_t>(q->needles[k].len)));
      if (chains.back().size() < min_chain) min_chain = chains.back().size();
    }
    if (min_chain >= 4) {
      for (auto& c : chains) { uint32_t t; std::memcpy(&t, c.data(), 4); t32.push_back(t); }
    } else if (min_chain >= 2) {
      for (auto& c : chains) { uint16_t t; std::memcpy(&t, c.data(), 2); t16.push_back(t); }
    }
    const bool passthrough = t32.empty() && t16.empty();

    const char* base = reinterpret_cast<const char*>(b_.compressed.data());
    if (passthrough) {
      // No usable chain for some needle: every row is a survivor, so there is
      // nothing to scan and no selection to materialize — straight to verify.
      for (uint64_t i = 0; i < b_.num_rows; i++) verify(q, base, i, out);
      if (stats) stats->prefilter_candidates = b_.num_rows;
      return 0;
    }

    uint32_t sel[VECTOR_SIZE];
    uint8_t marks[VECTOR_SIZE];
    uint64_t candidates = 0;
    // A vector at a time, as a DBMS pipeline does: the flat scan selects the
    // survivors of 2048 compressed rows, then each survivor is decompressed and
    // exactly checked.
    for (uint64_t row0 = 0; row0 < b_.num_rows; row0 += VECTOR_SIZE) {
      const uint32_t vn =
          static_cast<uint32_t>(std::min<uint64_t>(VECTOR_SIZE, b_.num_rows - row0));
      const uint32_t* voff = coff32_.data() + row0;
      const uint32_t nsel =
          !t32.empty()
              ? scan(base, voff, vn, t32.data(), static_cast<uint32_t>(t32.size()), sel, marks)
              : scan(base, voff, vn, t16.data(), static_cast<uint32_t>(t16.size()), sel, marks);
      candidates += nsel;
      for (uint32_t s = 0; s < nsel; s++) verify(q, base, row0 + sel[s], out);
    }
    if (stats) stats->prefilter_candidates = candidates;
    return 0;
  }

  uint32_t supported_ops() const override {
    return LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY);
  }

  void footprint(std::vector<lb_footprint_component>& out) const override {
    lb_footprint_component comps[3];
    const uint32_t n = fsst_common::Footprint(b_, comps, 3);
    // Undo the scan slack: `compressed` is oversized by BUFFER_SLACK so the
    // flat kernels can over-read the buffer end, which is not payload.
    for (uint32_t i = 0; i < n; i++) {
      if (std::strcmp(comps[i].name, "payload_fsst") == 0) comps[i].bytes = cbytes_;
      out.push_back(comps[i]);
    }
    out.push_back({"decode_table", sizeof(fsst_decoder_t)});
  }

  uint64_t num_rows() const override { return b_.num_rows; }

 private:
  // The correctness authority: decompress ONE survivor row and check the real
  // needles against the plaintext. The prefilter only ever narrows what gets
  // here, so this is what decides the output bitmap.
  void verify(const lb_query* q, const char* base, uint64_t i, uint64_t* out) {
    const char* cp = base + coff32_[i];
    const uint32_t clen = coff32_[i + 1] - coff32_[i];
    const size_t declen =
        fsst_decompress(&decoder_, clen, reinterpret_cast<const unsigned char*>(cp),
                        rowbuf_.size(), rowbuf_.data());
    const std::string_view row(reinterpret_cast<const char*>(rowbuf_.data()), declen);
    for (uint32_t k = 0; k < q->needle_count; k++) {
      const std::string_view needle(reinterpret_cast<const char*>(q->needles[k].ptr),
                                    static_cast<size_t>(q->needles[k].len));
      if (row.find(needle) != std::string_view::npos) {
        out[i >> 6] |= uint64_t{1} << (i & 63);
        return;
      }
    }
  }

  // One flat scan over a vector of rows: which of them contain ANY target code?
  // The vendored kernels take at most MAX_SYMBOLS codes per call (a wider k is
  // out of contract and aborts), and a CONTAINS_ANY query may carry more than
  // that, so the OR is split into groups of MAX_SYMBOLS and merged through the
  // mark array. Returns ascending vector-relative indices in `sel`.
  template <typename CodeT>
  static uint32_t scan(const char* base, const uint32_t* voff, uint32_t vn, const CodeT* codes,
                       uint32_t k, uint32_t* sel, uint8_t* marks) {
    if (k <= pf::MAX_SYMBOLS) return flat_simd(base, voff, vn, codes, k, sel);
    std::memset(marks, 0, vn);
    for (uint32_t g = 0; g < k; g += pf::MAX_SYMBOLS) {
      const uint32_t gk = std::min(pf::MAX_SYMBOLS, k - g);
      const uint32_t m = flat_simd(base, voff, vn, codes + g, gk, sel);
      for (uint32_t j = 0; j < m; j++) marks[sel[j]] = 1;
    }
    uint32_t nsel = 0;
    for (uint32_t i = 0; i < vn; i++) {
      sel[nsel] = i;
      nsel += marks[i];
    }
    return nsel;
  }

  // Width dispatch: the two kernels differ only in their code type.
  static uint32_t flat_simd(const char* base, const uint32_t* off, uint32_t n,
                            const uint32_t* codes, uint32_t k, uint32_t* sel) {
    return u32::flat::simd::contains(base, off, n, codes, k, sel);
  }
  static uint32_t flat_simd(const char* base, const uint32_t* off, uint32_t n,
                            const uint16_t* codes, uint32_t k, uint32_t* sel) {
    return u16::flat::simd::contains(base, off, n, codes, k, sel);
  }

  fsst_common::FsstBuilt b_;
  fsst_decoder_t decoder_{};
  std::vector<uint8_t> rowbuf_;
  std::vector<uint32_t> coff32_;  // num_rows + 1, 32-bit offsets for the flat scan
  uint64_t cbytes_ = 0;           // compressed payload without the scan slack
};

void* build_plain(const lb_chunk_view* view, const char*, char* eb, uint64_t ec) {
  return lb::adapter_build(std::make_unique<FsstPrefilter>(), view, eb, ec);
}
void* build_dict(const lb_chunk_view* view, const char*, char* eb, uint64_t ec) {
  auto child = std::make_unique<FsstPrefilter>();
  return lb::adapter_build(
      std::make_unique<lb::DictMatcher>(std::move(child), /*child_copies_input=*/true),
      view, eb, ec);
}

const uint32_t kOps = LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY);

const lb_strategy kPlainStrats[] = {{"fsst-prefilter", kOps}};
const lb_candidate kPlainVtable = {
    LB_ABI_VERSION, "fsst_prefilter", "0.3.0+e638d4c.resident-state", nullptr,
    kPlainStrats, 1, build_plain, lb::adapter_footprint, lb::adapter_run,
    nullptr, nullptr, lb::adapter_destroy};

const lb_strategy kDictStrats[] = {{"dict+fsst-prefilter", kOps}};
const lb_candidate kDictVtable = {
    LB_ABI_VERSION, "dict_fsst_prefilter", "0.3.0+e638d4c.resident-state", nullptr,
    kDictStrats, 1, build_dict, lb::adapter_footprint, lb::adapter_run,
    nullptr, nullptr, lb::adapter_destroy};

}  // namespace

extern "C" __attribute__((visibility("default"))) const lb_candidate*
lb_candidate_fsst_prefilter(void) {
  return &kPlainVtable;
}
extern "C" __attribute__((visibility("default"))) const lb_candidate*
lb_candidate_dict_fsst_prefilter(void) {
  return &kDictVtable;
}
