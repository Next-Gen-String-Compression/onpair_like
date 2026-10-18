// simd_cut_fsst_debug: simd_cut_fsst with two switches for ablations, over its own
// copy of our FSST fork (../fsst, bdaab1d plus the switches). The config is
// {"sorted": bool, "uniform": bool}, both optional:
//
//   "sorted"  (default true)  false skips fsst_sort_codes, so the codes keep FSST's own
//                             order; the planner then finds greedy steps and terminal
//                             symbols by a scan of the table and probes the terminal
//                             symbols as a set instead of a range.
//   "uniform" (default false) true plans with every code weighed codes / 256 instead
//                             of its count in the chunk.
//
// With both at their defaults it is simd_cut_fsst at that revision, less its
// dictionary peer.
//
// The walk is fused into the scan, so instrumented mode re-runs the phases
// beside it rather than splitting the fused pass: setup_ns (planning),
// prefilter_ns (the same cover scanned with the walk off, whose survivors are
// prefilter_candidates) and verify_ns (the alignment walks alone, over hits
// located beforehand and untimed). Each is the fastest of five runs after a
// warm-up, so a phase shorter than the clock's step is still resolved. The
// same split the simd_cut_onpair candidate reports, so the two ports' verify
// costs per candidate row compare directly. No scan_ns: the timed path is the
// fused scan, so the harness's median is already that number.
// The kernels are NEON on aarch64 and AVX-512BW or AVX2 on x86-64, chosen by
// -march=native at build time; the candidate declares the set it was built
// with and the harness gates on the host.

#include "lb_candidate.h"

#include "fsst.h"

#include "fsst_build.hpp"  // shared front-end; include AFTER fsst.h

#include "matcher.hpp"

#include "prefilter.hpp"

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstring>
#include <memory>
#include <string>
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

// A stage-two check for the instrumented pass. The resolver emits a row at
// its first passing hit and resumes past the row, so pf::scan::AcceptAll is
// the superset scan and a check that never passes enumerates every hit.
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

// The config's two switches.
struct Switches {
  bool sorted = true;
  bool uniform = false;
};

// {"sorted": bool, "uniform": bool}, both optional, or an empty config. Any other
// key, or a value other than true or false, is an error.
bool parse_switches(const char* json, Switches& out, char* eb, uint64_t ec) {
  auto fail = [&](const char* msg) {
    if (ec > 0) std::snprintf(eb, ec, "invalid config: %s", msg);
    return false;
  };
  const char* p = json ? json : "";
  auto ws = [&] {
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') ++p;
  };
  ws();
  if (*p == '\0') return true;
  if (*p++ != '{') return fail("expected an object");
  ws();
  if (*p == '}') return true;
  for (;;) {
    ws();
    if (*p++ != '"') return fail("expected a key");
    const char* key = p;
    while (*p && *p != '"') ++p;
    if (*p != '"') return fail("unterminated key");
    const std::string name(key, p++);
    ws();
    if (*p++ != ':') return fail("expected ':'");
    ws();
    bool value;
    if (std::strncmp(p, "true", 4) == 0) {
      value = true;
      p += 4;
    } else if (std::strncmp(p, "false", 5) == 0) {
      value = false;
      p += 5;
    } else {
      return fail("values are true or false");
    }
    if (name == "sorted") {
      out.sorted = value;
    } else if (name == "uniform") {
      out.uniform = value;
    } else {
      return fail("keys are \"sorted\" and \"uniform\"");
    }
    ws();
    if (*p == ',') {
      ++p;
      continue;
    }
    if (*p++ != '}') return fail("expected ',' or '}'");
    ws();
    return *p == '\0' || fail("trailing characters");
  }
}

class FsstPrefilter final : public lb::Matcher {
 public:
  bool build(const uint8_t* bytes, const uint64_t* offsets, uint64_t n, char* eb,
             uint64_t ec) override {
    const lb_chunk_view view{bytes, offsets, n};
    if (!fsst_common::Build(&view, b_, eb, ec, switches_.sorted ? fsst_sort_codes : nullptr)) return false;
    // The code counts travel inside the symbol table, as DuckDB stores them:
    // the planner reads both the same way.
    unsigned int counts[256] = {};
    for (uint8_t code : b_.compressed) counts[code]++;
    fsst_set_frequency(b_.enc, counts);
    b_.symtab.resize(FSST_MAXHEADER);
    b_.symtab.resize(fsst_export(b_.enc, b_.symtab.data()));
    fsst_destroy(b_.enc);
    b_.enc = nullptr;
    fsst_decoder_t decoder;
    if (fsst_import(&decoder, b_.symtab.data()) == 0) {
      if (ec > 0) std::snprintf(eb, ec, "fsst_import failed");
      return false;
    }
    dict_ = pf::Dictionary::from(decoder, pf::symbol_count(b_.symtab.data()));
    freq_ = pf::Frequency::of_counts(decoder.count);
    if (switches_.uniform) {
      // Every code weighs the stream's codes / 256, whatever the stream holds.
      uint32_t uniform[256];
      std::fill(std::begin(uniform), std::end(uniform), freq_.total / 256);
      freq_ = pf::Frequency::of_counts(uniform);
    }

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
    return true;
  }

  int run(const lb_query* q, uint64_t* out, lb_run_stats* stats) override {
    if (q->op == LB_PREFIX || q->op == LB_SUFFIX) {
      if (!prefix_suffix_) return 1;
      return run_prefix_suffix(q, out, stats);
    }
    if (q->op != LB_CONTAINS && q->op != LB_CONTAINS_ANY) return 1;

    // An empty needle among the set: every row matches, nothing to scan.
    const lb_selection* sel = q->selection;
    for (uint32_t k = 0; k < q->needle_count; k++) {
      if (q->needles[k].len == 0) {
        const uint64_t count = sel ? sel->count : b_.num_rows;
        for (uint64_t j = 0; j < count; j++) {
          const uint64_t i = sel ? sel->rows[j] : j;
          out[i >> 6] |= uint64_t{1} << (i & 63);
        }
        if (stats) stats->prefilter_candidates = count;
        return 0;
      }
    }

    // One planned scan per needle, ORed into the bitmap. Instrumented mode
    // re-runs the phases beside that scan; the timed work is identical either
    // way, and the harness never instruments a timed sample.
    // The phases time the scan over every row; a selection scan is not split.
    const bool timed = stats != nullptr && !sel;
    uint64_t setup_ns = 0, prefilter_ns = 0, verify_ns = 0, candidates = 0;
    const pf::scan::Stream stream{b_.compressed.data(), b_.compressed.size(), coff32_.data(), coff32_.size()};
    for (uint32_t k = 0; k < q->needle_count; k++) {
      const uint8_t* needle = q->needles[k].ptr;
      const size_t nlen = static_cast<size_t>(q->needles[k].len);
      const pf::Analysis a = pf::analyze(needle, nlen, dict_, freq_, b_.num_rows);
      const pf::scan::WalkCheck walk{a.walk, dict_, stream.codes};

      rows_.clear();
      if (sel)
        finder_.find(stream, dict_, a, sel->rows, sel->count, rows_);
      else
        pf::execute(a, stream, walk, rows_);

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
          pf::execute(a, stream, pf::scan::AcceptAll{}, survivors_);
        });
        candidates += survivors_.size();

        // The walks alone, over the hits the kernels produce. Locating them is
        // the scan's job and is charged to prefilter_ns, so it runs once here,
        // untimed. The collector never passes, so nothing lands in `sink`.
        hits_.clear();
        std::vector<size_t> sink;
        pf::execute(a, stream, HitCollector{hits_}, sink);
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
    const uint32_t ops = LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY);
    return prefix_suffix_ ? ops | LB_OP_BIT(LB_PREFIX) | LB_OP_BIT(LB_SUFFIX) : ops;
  }

  explicit FsstPrefilter(Switches switches) : prefix_suffix_(true), switches_(switches) {}

  void footprint(std::vector<lb_footprint_component>& out) const override {
    lb_footprint_component comps[3];
    const uint32_t n = fsst_common::Footprint(b_, comps, 3);
    for (uint32_t i = 0; i < n; i++) out.push_back(comps[i]);
    // The frequency index the planner reads: code counts and their
    // cumulative sum.
    out.push_back({"prefilter", sizeof(freq_.count) + sizeof(freq_.cum)});
  }

  uint64_t num_rows() const override { return b_.num_rows; }

 private:
  template <pf::Anchor A>
  void anchored_rows(const pf::Analysis& a, const lb_selection* sel, uint64_t* out) const {
    const uint8_t* codes = b_.compressed.data();
    const uint64_t count = sel ? sel->count : b_.num_rows;
    for (uint64_t j = 0; j < count; j++) {
      const uint64_t i = sel ? sel->rows[j] : j;
      if (a.matches_all || a.walk.check_anchored<A>(dict_, codes, coff32_[i], coff32_[i + 1]))
        out[i >> 6] |= uint64_t{1} << (i & 63);
    }
  }

  int run_prefix_suffix(const lb_query* q, uint64_t* out, lb_run_stats* stats) {
    const uint8_t* needle = q->needles[0].ptr;
    const size_t nlen = static_cast<size_t>(q->needles[0].len);
    const pf::Anchor anchor = q->op == LB_PREFIX ? pf::Anchor::Start : pf::Anchor::End;
    const pf::Analysis a = pf::analyze(needle, nlen, dict_, freq_, b_.num_rows, anchor);
    if (anchor == pf::Anchor::Start)
      anchored_rows<pf::Anchor::Start>(a, q->selection, out);
    else
      anchored_rows<pf::Anchor::End>(a, q->selection, out);
    if (stats) {
      stats->setup_ns = best_ns([&] {
        const pf::Analysis again = pf::analyze(needle, nlen, dict_, freq_, b_.num_rows, anchor);
        keep(&again);
      });
    }
    return 0;
  }

  bool prefix_suffix_;
  Switches switches_;
  fsst_common::FsstBuilt b_;
  pf::Dictionary dict_;
  pf::Frequency freq_;
  std::vector<uint32_t> coff32_;  // num_rows + 1, 32-bit offsets into the code stream
  pf::Finder finder_;             // the selection scan's scratch, reused by every query
  std::vector<size_t> rows_;      // scratch: the rows of one needle
  std::vector<size_t> survivors_; // scratch: the superset scan's rows, instrumented mode
  std::vector<Hit> hits_;         // scratch: the hits the walk is timed over
};

void* build_plain(const lb_chunk_view* view, const char* config, char* eb, uint64_t ec) {
  Switches switches;
  if (!parse_switches(config, switches, eb, ec)) return nullptr;
  return lb::adapter_build(std::make_unique<FsstPrefilter>(switches), view, eb, ec);
}

const uint32_t kOps = LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY);
const uint32_t kPlainOps = kOps | LB_OP_BIT(LB_PREFIX) | LB_OP_BIT(LB_SUFFIX);

#if defined(__ARM_NEON)
const char* const kCpuFeatures = "neon";
#elif defined(__AVX512BW__)
const char* const kCpuFeatures = "avx512f,avx512bw";
#else
const char* const kCpuFeatures = "avx2";
#endif

const lb_strategy kPlainStrats[] = {{"simd-cut", kPlainOps, LB_STRATEGY_TAKES_SELECTION}};
const lb_candidate kPlainVtable = {
    LB_ABI_VERSION, "simd_cut_fsst_debug", "0.4.0-debug", kCpuFeatures,
    kPlainStrats, 1, build_plain, lb::adapter_footprint, lb::adapter_run,
    nullptr, nullptr, lb::adapter_destroy};

}  // namespace

extern "C" __attribute__((visibility("default"))) const lb_candidate*
lb_candidate_simd_cut_fsst_debug(void) {
  return &kPlainVtable;
}
