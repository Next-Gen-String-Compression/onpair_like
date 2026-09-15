// fsst_prefilter: exact LIKE '%needle%' in the FSST-compressed domain, and its
// dictionary peer dict_fsst_prefilter. Two candidates from one FSST copy (this
// file exports both entry points; CMakeLists localizes the rest).
//
//   1. build()  trains FSST, renumbers the codes into byte order of their
//      symbols (fsst_sort_codes), compresses every row whole, and counts the
//      codes and code pairs of the stream once.
//   2. per query, the planner (fork: search/prefilter) builds the alignment
//      graph of the needle over the symbol table, every way it can lie across
//      symbol boundaries, and takes the min cut whose cover the fitted cost
//      model prices lowest: codes, code ranges and code pairs.
//   3. the compressed stream is scanned for the cover with a NEON kernel, hits
//      are resolved to rows, and every hit is verified by the alignment walk
//      over the surrounding codes. No row is decompressed.
//
//   dict_fsst_prefilter : the same matcher as a DictMatcher child, so planning
//                         and the scan run once per UNIQUE value.
//
// The walk is exact, so there is no verify phase to time apart from the scan:
// instrumented mode reports setup_ns (planning), scan_ns (the scan with the
// walk inline) and prefilter_candidates (rows the mask named, before the walk).
// NEON only until the AVX2 kernels exist.

#include "lb_candidate.h"

#include "fsst.h"

#include "fsst_build.hpp"  // shared front-end; include AFTER fsst.h

#include "matcher.hpp"
#include "dict_matcher.hpp"

#include "prefilter.hpp"

#include <chrono>
#include <cstdint>
#include <cstring>
#include <memory>
#include <vector>

namespace {

namespace pf = fsst::search::prefilter;

// The walk as stage two's check, counting the rows it was asked about. Only
// instantiated in instrumented mode; timing mode runs the plain WalkCheck.
struct CountingWalk {
  pf::scan::WalkCheck inner;
  mutable uint64_t rows = 0;
  mutable size_t last_row = SIZE_MAX;

  bool passes(size_t hit, size_t row_start, size_t row_end) const {
    if (row_start != last_row) {
      last_row = row_start;
      ++rows;
    }
    return inner.passes(hit, row_start, row_end);
  }
};

class FsstPrefilter final : public lb::Matcher {
 public:
  bool build(const uint8_t* bytes, const uint64_t* offsets, uint64_t n, char* eb,
             uint64_t ec) override {
    const lb_chunk_view view{bytes, offsets, n};
    if (!fsst_common::Build(&view, b_, eb, ec, fsst_sort_codes)) return false;
    fsst_decoder_t decoder;
    if (fsst_import(&decoder, b_.symtab.data()) == 0) {
      fsst_destroy(b_.enc);
      b_.enc = nullptr;
      if (ec > 0) std::snprintf(eb, ec, "fsst_import failed");
      return false;
    }
    fsst_destroy(b_.enc);
    b_.enc = nullptr;
    dict_ = pf::Dictionary::from(decoder.symbol, decoder.len, pf::symbol_count(b_.symtab.data()));

    // The scan indexes one contiguous code stream with 32-bit row offsets, so
    // the u64 compressed-row offsets are narrowed once here and released.
    const uint64_t cbytes = b_.coffsets[n];
    if (cbytes > uint64_t{UINT32_MAX}) {
      if (ec > 0)
        std::snprintf(eb, ec, "compressed chunk is %llu B, past the scan's 32-bit offsets",
                      static_cast<unsigned long long>(cbytes));
      return false;
    }
    coff32_.resize(n + 1);
    for (uint64_t i = 0; i <= n; i++) coff32_[i] = static_cast<uint32_t>(b_.coffsets[i]);
    b_.coffsets.clear();
    b_.coffsets.shrink_to_fit();
    freq_ = pf::Frequency::of(b_.compressed.data(), b_.compressed.size());
    return true;
  }

  int run(const lb_query* q, uint64_t* out, lb_run_stats* stats) override {
    if (q->op != LB_CONTAINS && q->op != LB_CONTAINS_ANY) return 1;
    for (uint32_t k = 0; k < q->needle_count; k++)
      if (q->needles[k].len > pf::MAX_PATTERN_LEN) return 1;

    // An empty needle among the set: every row matches, nothing to scan.
    for (uint32_t k = 0; k < q->needle_count; k++) {
      if (q->needles[k].len == 0) {
        for (uint64_t i = 0; i < b_.num_rows; i++) out[i >> 6] |= uint64_t{1} << (i & 63);
        if (stats) stats->prefilter_candidates = b_.num_rows;
        return 0;
      }
    }

    // One planned scan per needle, ORed into the bitmap. The clock is read
    // only in instrumented mode; the timed work is identical either way.
    using clk = std::chrono::steady_clock;
    const bool timed = stats != nullptr;
    uint64_t setup_ns = 0, scan_ns = 0, candidates = 0;
    const uint8_t* codes = b_.compressed.data();
    const size_t len = b_.compressed.size();
    for (uint32_t k = 0; k < q->needle_count; k++) {
      const auto t_plan = timed ? clk::now() : clk::time_point{};
      pf::Analysis a = pf::analyze(q->needles[k].ptr, static_cast<size_t>(q->needles[k].len), dict_,
                                   freq_, b_.num_rows);
      const auto t_scan = timed ? clk::now() : clk::time_point{};
      rows_.clear();
      if (timed) {
        CountingWalk check{{a.walk, dict_, codes}};
        pf::scan::execute(a.cover, codes, len, coff32_.data(), coff32_.size(), a.covered_frequency,
                          a.total_frequency, check, rows_);
        candidates += check.rows;
        const auto t_end = clk::now();
        setup_ns += static_cast<uint64_t>(
            std::chrono::duration_cast<std::chrono::nanoseconds>(t_scan - t_plan).count());
        scan_ns += static_cast<uint64_t>(
            std::chrono::duration_cast<std::chrono::nanoseconds>(t_end - t_scan).count());
      } else {
        pf::scan::execute(a.cover, codes, len, coff32_.data(), coff32_.size(), a.covered_frequency,
                          a.total_frequency, pf::scan::WalkCheck{a.walk, dict_, codes}, rows_);
      }
      for (size_t row : rows_) out[row >> 6] |= uint64_t{1} << (row & 63);
    }
    if (stats) {
      stats->prefilter_candidates = candidates;
      stats->setup_ns = setup_ns;
      stats->scan_ns = scan_ns;
    }
    return 0;
  }

  uint32_t supported_ops() const override {
    return LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY);
  }

  void footprint(std::vector<lb_footprint_component>& out) const override {
    lb_footprint_component comps[3];
    const uint32_t n = fsst_common::Footprint(b_, comps, 3);
    for (uint32_t i = 0; i < n; i++) out.push_back(comps[i]);
    // The frequency index the planner reads: code counts, their cumulative
    // sum and the code pair table.
    out.push_back({"prefilter", sizeof(freq_.count) + sizeof(freq_.cum) +
                                    freq_.pairs.size() * sizeof(uint32_t)});
  }

  uint64_t num_rows() const override { return b_.num_rows; }

 private:
  fsst_common::FsstBuilt b_;
  pf::Dictionary dict_;
  pf::Frequency freq_;
  std::vector<uint32_t> coff32_;  // num_rows + 1, 32-bit offsets into the code stream
  std::vector<size_t> rows_;      // scratch: the rows of one needle
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
    LB_ABI_VERSION, "fsst_prefilter", "0.3.0+e0ec68b", "neon",
    kPlainStrats, 1, build_plain, lb::adapter_footprint, lb::adapter_run,
    nullptr, nullptr, lb::adapter_destroy};

const lb_strategy kDictStrats[] = {{"dict+fsst-prefilter", kOps}};
const lb_candidate kDictVtable = {
    LB_ABI_VERSION, "dict_fsst_prefilter", "0.3.0+e0ec68b", "neon",
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
