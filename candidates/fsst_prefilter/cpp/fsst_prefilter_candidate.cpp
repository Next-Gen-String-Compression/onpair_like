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
//   3. scan every row's COMPRESSED bytes for that code with the byte-domain
//      prefilter (prefilter.hpp). No decompression on rejected rows.
//   4. decompress only the SURVIVORS (per row) and verify the real needle.
// This mirrors DuckDB's ContainsPrefilterExecptor FSST path (see
// candidates/common/fsst_common/table_filter_state.cpp): mandatory-chain codes
// -> PrefilterContainsAny on the compressed view -> exact filter downstream.
//
//   dict_fsst_prefilter : the same matcher as a DictMatcher child, so chain
//                         building + the compressed scan run once per UNIQUE value.
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
#include "prefilter.hpp"

#include <cstdint>
#include <cstring>
#include <memory>
#include <string_view>
#include <vector>

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

    uint64_t candidates = 0;
    for (uint64_t i = 0; i < b_.num_rows; i++) {
      const char* cp = reinterpret_cast<const char*>(b_.compressed.data()) + b_.coffsets[i];
      const uint32_t clen = static_cast<uint32_t>(b_.coffsets[i + 1] - b_.coffsets[i]);
      // Compressed-domain prefilter: does this row's code stream contain any target?
      bool pf = passthrough;
      if (!pf) {
        if (!t32.empty()) {
          for (uint32_t c : t32) if (prefilter::contains<uint32_t>(cp, clen, c)) { pf = true; break; }
        } else {
          for (uint16_t c : t16) if (prefilter::contains<uint16_t>(cp, clen, c)) { pf = true; break; }
        }
      }
      if (!pf) continue;
      candidates++;
      // Verify: decompress this one row, then exact substring check.
      const size_t declen = fsst_decompress(
          &decoder_, clen, reinterpret_cast<const unsigned char*>(cp), rowbuf_.size(),
          rowbuf_.data());
      const std::string_view row(reinterpret_cast<const char*>(rowbuf_.data()), declen);
      bool hit = false;
      for (uint32_t k = 0; k < q->needle_count && !hit; k++) {
        const std::string_view needle(reinterpret_cast<const char*>(q->needles[k].ptr),
                                      static_cast<size_t>(q->needles[k].len));
        hit = row.find(needle) != std::string_view::npos;
      }
      if (hit) out[i >> 6] |= uint64_t{1} << (i & 63);
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
    for (uint32_t i = 0; i < n; i++) out.push_back(comps[i]);
  }

  uint64_t num_rows() const override { return b_.num_rows; }

 private:
  fsst_common::FsstBuilt b_;
  fsst_decoder_t decoder_{};
  std::vector<uint8_t> rowbuf_;
};

void* build_plain(const lb_chunk_view* view, const char*, char* eb, uint64_t ec) {
  return lb::adapter_build(std::make_unique<FsstPrefilter>(), view, eb, ec);
}
void* build_dict(const lb_chunk_view* view, const char*, char* eb, uint64_t ec) {
  auto child = std::make_unique<FsstPrefilter>();
  return lb::adapter_build(std::make_unique<lb::DictMatcher>(std::move(child)), view, eb, ec);
}

const uint32_t kOps = LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY);

const lb_strategy kPlainStrats[] = {{"fsst-prefilter", kOps}};
const lb_candidate kPlainVtable = {
    LB_ABI_VERSION, "fsst_prefilter", "0.1.0+e638d4c", nullptr,
    kPlainStrats, 1, build_plain, lb::adapter_footprint, lb::adapter_run,
    nullptr, nullptr, lb::adapter_destroy};

const lb_strategy kDictStrats[] = {{"dict+fsst-prefilter", kOps}};
const lb_candidate kDictVtable = {
    LB_ABI_VERSION, "dict_fsst_prefilter", "0.1.0+e638d4c", nullptr,
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
