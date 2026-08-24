// fsst_decode_prefilter — FSST storage with a DECODE-then-PREFILTER evaluator,
// and its dictionary peer dict_fsst_decode_prefilter. Two candidates from one
// FSST copy (this file exports both entry points; CMakeLists localizes the rest).
//
//   fsst_decode_prefilter      : build() FSST-compresses the column; run()
//                                bulk-decodes to plaintext, then answers the
//                                query with the code-prefilter (extract a 2/4-byte
//                                tail code, reject with prefilter.hpp, verify with
//                                find). The decompress-then-prefilter peer of the
//                                decode-only `fsst` + prefilter scanner, folded
//                                into one self-contained candidate.
//   dict_fsst_decode_prefilter : the same matcher as a DictMatcher child, so the
//                                FSST compress + decode + prefilter runs once per
//                                UNIQUE value (encoded domain) and scatters to rows.
//
// Uses cwida/fsst upstream (the `fsst` candidate's fork) via the shared
// fsst_common front-end; because that is a SECOND cwida FSST copy in the
// harness, every FSST symbol here is localized at link (CMakeLists.txt) leaving
// only the two lb_candidate_* entry points exported.

#include "lb_candidate.h"

#include "fsst.h"

#include "fsst_build.hpp"  // shared front-end; include AFTER fsst.h

#include "matcher.hpp"
#include "dict_matcher.hpp"
#include "uncompressed_matchers.hpp"

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <memory>
#include <vector>

namespace {

// FSST storage + decode-then-prefilter evaluation, behind lb::Matcher.
class FsstDecodePrefilter final : public lb::Matcher {
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
    // Reusable decode scratch (allocation persists; contents carry nothing).
    // +LB_DECODE_PAD covers FSST's fixed-stride over-copy past the last row.
    scratch_.resize(b_.payload_bytes + LB_DECODE_PAD);
    // The prefilter evaluates over the decoded plaintext; its pointers into the
    // scratch/offsets are stable across runs (only the scratch bytes change).
    pf_.build(scratch_.data(), b_.offsets.data(), b_.num_rows, nullptr, 0);
    // Bulk decode needs canonical decoded boundaries, not compressed-row
    // boundaries into the concatenated code stream.
    fsst_common::ReleaseCompressedOffsets(b_);
    return true;
  }

  int run(const lb_query* q, uint64_t* out, lb_run_stats* stats) override {
    using Clock = std::chrono::steady_clock;
    const auto t0 = stats ? Clock::now() : Clock::time_point{};
    const size_t got = fsst_decompress(&decoder_, b_.compressed.size(),
                                       b_.compressed.data(), scratch_.size(),
                                       scratch_.data());
    if (got != b_.payload_bytes) return 3;
    if (stats) {
      stats->decode_ns = uint64_t(std::chrono::duration_cast<std::chrono::nanoseconds>(
                                      Clock::now() - t0)
                                      .count());
    }
    return pf_.run(q, out, stats);
  }

  uint32_t supported_ops() const override {
    return LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY);
  }

  void footprint(std::vector<lb_footprint_component>& out) const override {
    lb_footprint_component comps[3];
    const uint32_t n = fsst_common::Footprint(b_, comps, 3);
    for (uint32_t i = 0; i < n; i++) out.push_back(comps[i]);
    out.push_back({"decode_table", sizeof(fsst_decoder_t)});
  }

  uint64_t num_rows() const override { return b_.num_rows; }

 private:
  fsst_common::FsstBuilt b_;
  fsst_decoder_t decoder_{};
  std::vector<uint8_t> scratch_;
  lb::UncompressedPrefilter pf_;
};

void* build_plain(const lb_chunk_view* view, const char*, char* eb, uint64_t ec) {
  return lb::adapter_build(std::make_unique<FsstDecodePrefilter>(), view, eb, ec);
}

void* build_dict(const lb_chunk_view* view, const char*, char* eb, uint64_t ec) {
  auto child = std::make_unique<FsstDecodePrefilter>();
  return lb::adapter_build(
      std::make_unique<lb::DictMatcher>(std::move(child), /*child_copies_input=*/true),
      view, eb, ec);
}

const uint32_t kOps = LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY);

const lb_strategy kPlainStrats[] = {{"decode+prefilter", kOps}};
const lb_candidate kPlainVtable = {
    LB_ABI_VERSION, "fsst_decode_prefilter", "0.2.0+e638d4c.resident-state", nullptr,
    kPlainStrats, 1, build_plain, lb::adapter_footprint, lb::adapter_run,
    nullptr, nullptr, lb::adapter_destroy};

const lb_strategy kDictStrats[] = {{"dict+decode+prefilter", kOps}};
const lb_candidate kDictVtable = {
    LB_ABI_VERSION, "dict_fsst_decode_prefilter", "0.2.0+e638d4c.resident-state", nullptr,
    kDictStrats, 1, build_dict, lb::adapter_footprint, lb::adapter_run,
    nullptr, nullptr, lb::adapter_destroy};

}  // namespace

extern "C" __attribute__((visibility("default"))) const lb_candidate*
lb_candidate_fsst_decode_prefilter(void) {
  return &kPlainVtable;
}

extern "C" __attribute__((visibility("default"))) const lb_candidate*
lb_candidate_dict_fsst_decode_prefilter(void) {
  return &kDictVtable;
}
