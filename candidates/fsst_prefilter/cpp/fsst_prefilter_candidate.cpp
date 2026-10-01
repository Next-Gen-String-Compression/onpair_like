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
// The walk is fused into the scan, so instrumented mode re-runs the phases
// beside it rather than splitting the fused pass: setup_ns (planning),
// prefilter_ns (the same cover scanned with the walk off, whose survivors are
// prefilter_candidates) and verify_ns (the alignment walks alone, over hits
// located beforehand and untimed). Each is the fastest of five runs after a
// warm-up, so a phase shorter than the clock's step is still resolved. The
// same split the onpair_spiral candidate reports, so the two ports' verify
// costs per candidate row compare directly. No scan_ns: the timed path is the
// fused scan, so the harness's median is already that number.
// The kernels are NEON on aarch64 and AVX-512BW or AVX2 on x86-64, chosen by
// -march=native at build time; the candidate declares the set it was built
// with and the harness gates on the host.

#include "lb_candidate.h"

#include "fsst.h"

#include "fsst_build.hpp"  // shared front-end; include AFTER fsst.h

#include "matcher.hpp"
#include "dict_matcher.hpp"

#include "prefilter.hpp"

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstring>
#include <memory>
#include <vector>

namespace {

namespace pf = fsst::search::prefilter;

// Keeps a phase whose result the timing loop does not otherwise use from
// being optimized away.
inline void keep(const void* p) { asm volatile("" : : "r"(p) : "memory"); }

// Nanoseconds of the fastest of five runs, after one untimed; a region under
// the floor is repeated inside the timer until it clears and the time divided
// back. A phase measured once, cold, against another measured once, warm,
// reports the order the two ran in as much as the work they did.
template <typename F>
uint64_t best_ns(F&& run) {
  using clk = std::chrono::steady_clock;
  constexpr int kPasses = 5;
  constexpr uint64_t kFloorNs = 1000;
  run();
  for (uint64_t reps = 1;; reps *= 2) {
    uint64_t best = UINT64_MAX;
    for (int pass = 0; pass < kPasses; pass++) {
      const auto t0 = clk::now();
      for (uint64_t i = 0; i < reps; i++) run();
      best = std::min(best, static_cast<uint64_t>(std::chrono::duration_cast<std::chrono::nanoseconds>(
                                                      clk::now() - t0)
                                                      .count()));
    }
    if (best > kFloorNs || reps == (uint64_t{1} << 20)) return best / reps;
  }
}

// Stage-two policies for the instrumented pass. The resolver emits a row at
// its first passing hit and resumes past the row, so a check that always
// passes is the superset scan and one that never passes enumerates every hit.
struct SupersetCheck {
  bool passes(size_t, size_t, size_t) const { return true; }
};

struct Hit {
  size_t code, row_start, row_end;
};

struct HitCollector {
  std::vector<Hit>& out;
  bool passes(size_t code, size_t row_start, size_t row_end) const {
    out.push_back({code, row_start, row_end});
    return false;
  }
};

// The walk alone, over hits the scan already located, deciding a row at its
// first passing hit and skipping the rest of it as the resolver does.
size_t walk_hits(const pf::scan::WalkCheck& walk, const std::vector<Hit>& hits) {
  size_t rows = 0;
  for (size_t i = 0; i < hits.size();) {
    const size_t start = hits[i].row_start;
    bool passed = false;
    for (; i < hits.size() && hits[i].row_start == start; i++) {
      if (walk.passes(hits[i].code, start, hits[i].row_end)) {
        passed = true;
        break;
      }
    }
    if (passed) {
      rows++;
      while (i < hits.size() && hits[i].row_start == start) i++;
    }
  }
  return rows;
}

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
    // `symbol` is `unsigned long long`, which is `uint64_t` on macOS but not on
    // LP64, where the two are distinct types of the same width.
    dict_ = pf::Dictionary::from(reinterpret_cast<const uint64_t*>(decoder.symbol), decoder.len,
                                 pf::symbol_count(b_.symtab.data()));

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

    // One planned scan per needle, ORed into the bitmap. Instrumented mode
    // re-runs the phases beside that scan; the timed work is identical either
    // way, and the harness never instruments a timed sample.
    const bool timed = stats != nullptr;
    uint64_t setup_ns = 0, prefilter_ns = 0, verify_ns = 0, candidates = 0;
    const uint8_t* codes = b_.compressed.data();
    const size_t len = b_.compressed.size();
    for (uint32_t k = 0; k < q->needle_count; k++) {
      const uint8_t* needle = q->needles[k].ptr;
      const size_t nlen = static_cast<size_t>(q->needles[k].len);
      const pf::Analysis a = pf::analyze(needle, nlen, dict_, freq_, b_.num_rows);
      const pf::scan::WalkCheck walk{a.walk, dict_, codes};
      auto scan = [&](const auto& check, std::vector<size_t>& into) {
        pf::scan::execute(a.cover, codes, len, coff32_.data(), coff32_.size(), a.covered_frequency,
                          a.total_frequency, check, into);
      };

      rows_.clear();
      scan(walk, rows_);

      if (timed) {
        setup_ns += best_ns([&] {
          const pf::Analysis again = pf::analyze(needle, nlen, dict_, freq_, b_.num_rows);
          keep(&again);
        });

        // The same cover with the walk off: its survivors are the denominator
        // of the prune and false-positive rates, and its time is the scan the
        // walk sits behind.
        prefilter_ns += best_ns([&] {
          survivors_.clear();
          scan(SupersetCheck{}, survivors_);
        });
        candidates += survivors_.size();

        // The walks alone, over the hits the kernels produce. Locating them is
        // the scan's job and is charged to prefilter_ns, so it runs once here,
        // untimed. The collector never passes, so nothing lands in `sink`.
        hits_.clear();
        std::vector<size_t> sink;
        scan(HitCollector{hits_}, sink);
        verify_ns += best_ns([&] {
          const size_t n = walk_hits(walk, hits_);
          keep(&n);
        });
      }
      for (size_t row : rows_) out[row >> 6] |= uint64_t{1} << (row & 63);
    }
    if (stats) {
      stats->prefilter_candidates = candidates;
      stats->setup_ns = setup_ns;
      stats->prefilter_ns = prefilter_ns;
      stats->verify_ns = verify_ns;
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
  std::vector<size_t> survivors_; // scratch: the superset scan's rows, instrumented mode
  std::vector<Hit> hits_;         // scratch: the hits the walk is timed over
};

void* build_plain(const lb_chunk_view* view, const char*, char* eb, uint64_t ec) {
  return lb::adapter_build(std::make_unique<FsstPrefilter>(), view, eb, ec);
}
void* build_dict(const lb_chunk_view* view, const char*, char* eb, uint64_t ec) {
  auto child = std::make_unique<FsstPrefilter>();
  return lb::adapter_build(std::make_unique<lb::DictMatcher>(std::move(child)), view, eb, ec);
}

const uint32_t kOps = LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY);

#if defined(__ARM_NEON)
const char* const kCpuFeatures = "neon";
#elif defined(__AVX512BW__)
const char* const kCpuFeatures = "avx512f,avx512bw";
#else
const char* const kCpuFeatures = "avx2";
#endif

const lb_strategy kPlainStrats[] = {{"fsst-prefilter", kOps}};
const lb_candidate kPlainVtable = {
    LB_ABI_VERSION, "fsst_prefilter", "0.3.0+ecc636d", kCpuFeatures,
    kPlainStrats, 1, build_plain, lb::adapter_footprint, lb::adapter_run,
    nullptr, nullptr, lb::adapter_destroy};

const lb_strategy kDictStrats[] = {{"dict+fsst-prefilter", kOps}};
const lb_candidate kDictVtable = {
    LB_ABI_VERSION, "dict_fsst_prefilter", "0.3.0+ecc636d", kCpuFeatures,
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
