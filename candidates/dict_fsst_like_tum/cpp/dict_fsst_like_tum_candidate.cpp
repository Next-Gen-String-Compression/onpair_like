// dict_fsst_like_tum — dictionary encoding in the ENCODED domain, with the
// FSST-LIKE compressed-domain matcher (calin2110/FSST-LIKE-Matching) as its
// child. build() deduplicates the chunk into unique values, then FSST-trains +
// compresses the UNIQUE values and builds the LIKE automaton over them; run()
// drives the automaton once per unique value and scatters the result to rows
// through the per-row codes.
//
// The FSST-LIKE engine here is exactly the one in the `fsst_like_tum` candidate
// (same pinned fork + shared fsst_common front-end), lifted behind the lb::
// Matcher interface (candidates/common/matcher) so the dictionary front-end can
// compose it. As with fsst_like_tum, this candidate statically links its own
// calin2110 FSST copy, so every FSST / FSST-LIKE symbol is localized at link
// (CMakeLists.txt) leaving only lb_candidate_dict_fsst_like_tum exported.
//
// CONTAINS_ANY is unsupported (an OR of literals is not one LIKE pattern); the
// literal-backslash-tail limitation of the parser is inherited verbatim.

#include "lb_candidate.h"

// libfsst.hpp has NO include guard; encoder.hpp includes it exactly once.
#include "encoder.hpp"
#include "like_pattern_automaton.hpp"
#include <fsst/fsst.h>

#include "fsst_build.hpp"  // shared FSST front-end; include AFTER <fsst/fsst.h>

#include "matcher.hpp"
#include "dict_matcher.hpp"

#include <chrono>
#include <cstdint>
#include <cstring>
#include <memory>
#include <span>
#include <string>
#include <vector>

namespace {

// Append `n` bytes to `out` with LIKE metacharacters escaped (literal match).
void escape_append(std::vector<uint8_t>& out, const uint8_t* p, uint64_t len) {
  for (uint64_t i = 0; i < len; i++) {
    const uint8_t c = p[i];
    if (c == '%' || c == '_' || c == '\\') out.push_back('\\');
    out.push_back(c);
  }
}

// (op, needles) -> LIKE pattern bytes. False for CONTAINS_ANY (not one pattern).
bool to_like_pattern(const lb_query* q, std::vector<uint8_t>& pat) {
  switch (q->op) {
    case LB_PREFIX:
      escape_append(pat, q->needles[0].ptr, q->needles[0].len);
      pat.push_back('%');
      return true;
    case LB_SUFFIX:
      pat.push_back('%');
      escape_append(pat, q->needles[0].ptr, q->needles[0].len);
      return true;
    case LB_CONTAINS:
      pat.push_back('%');
      escape_append(pat, q->needles[0].ptr, q->needles[0].len);
      pat.push_back('%');
      return true;
    case LB_MULTI_CONTAINS:
      pat.push_back('%');
      for (uint32_t i = 0; i < q->needle_count; i++) {
        escape_append(pat, q->needles[i].ptr, q->needles[i].len);
        pat.push_back('%');
      }
      return true;
    default:
      return false;
  }
}

// The FSST-LIKE compressed-domain matcher over one column, lifted behind lb::
// Matcher. Mirrors the fsst_like_tum candidate's build()/run().
class FsstLikeTum final : public lb::Matcher {
 public:
  bool build(const uint8_t* bytes, const uint64_t* offsets, uint64_t n, char* eb,
             uint64_t ec) override {
    const lb_chunk_view view{bytes, offsets, n};
    if (!fsst_common::Build(&view, b_, eb, ec)) return false;
    try {
      std::shared_ptr<libfsst::SymbolTable> sym =
          reinterpret_cast<libfsst::Encoder*>(b_.enc)->symbolTable;
      // Escaped-byte bitmap into symbols[255], as the repo's compressFile does.
      bool bitmap[256] = {false};
      for (uint64_t i = 0; i < b_.num_rows; i++) {
        const uint8_t* p = b_.compressed.data() + b_.coffsets[i];
        const size_t len = size_t(b_.coffsets[i + 1] - b_.coffsets[i]);
        size_t j = 0;
        while (j < len) {
          if (p[j] == 255) { ++j; bitmap[p[j]] = true; }
          ++j;
        }
      }
      std::memcpy(&sym->symbols[255], bitmap, 256 * sizeof(bool));
      SymbolTable st(sym);
      encoder_ = std::make_unique<Encoder>(st);
      fsst_destroy(b_.enc);
      b_.enc = nullptr;
      // Compressed-domain execution needs only compressed-row boundaries.
      fsst_common::ReleaseDecodedOffsets(b_);
      return true;
    } catch (const std::exception& e) {
      if (b_.enc) { fsst_destroy(b_.enc); b_.enc = nullptr; }
      if (ec > 0) std::snprintf(eb, ec, "build failed: %s", e.what());
      return false;
    } catch (...) {
      if (b_.enc) { fsst_destroy(b_.enc); b_.enc = nullptr; }
      if (ec > 0) std::snprintf(eb, ec, "build failed: unknown exception");
      return false;
    }
  }

  int run(const lb_query* q, uint64_t* out, lb_run_stats* stats) override {
    std::vector<uint8_t> pat;
    if (!to_like_pattern(q, pat)) return 11;
    using Clock = std::chrono::steady_clock;
    const auto t0 = stats ? Clock::now() : Clock::time_point{};
    try {
      automata::parsing::LikePatternAutomatonParser parser(
          std::span<const uint8_t>(pat.data(), pat.size()), *encoder_);
      if (stats) {
        stats->setup_ns = uint64_t(std::chrono::duration_cast<std::chrono::nanoseconds>(
                                       Clock::now() - t0)
                                       .count());
      }
      for (uint64_t i = 0; i < b_.num_rows; i++) {
        const uint8_t* p = b_.compressed.data() + b_.coffsets[i];
        const size_t len = size_t(b_.coffsets[i + 1] - b_.coffsets[i]);
        if (parser.parse(std::span<const uint8_t>(p, len)))
          out[i >> 6] |= uint64_t(1) << (i & 63);
      }
      return 0;
    } catch (...) {
      return 12;
    }
  }

  uint32_t supported_ops() const override {
    return LB_OP_BIT(LB_PREFIX) | LB_OP_BIT(LB_SUFFIX) | LB_OP_BIT(LB_CONTAINS) |
           LB_OP_BIT(LB_MULTI_CONTAINS);
  }

  void footprint(std::vector<lb_footprint_component>& out) const override {
    lb_footprint_component comps[3];
    const uint32_t n = fsst_common::Footprint(b_, comps, 3);
    for (uint32_t i = 0; i < n; i++) out.push_back(comps[i]);
    out.push_back({"encoder_table",
                   sizeof(libfsst::Encoder) + sizeof(libfsst::SymbolTable)});
  }

  uint64_t num_rows() const override { return b_.num_rows; }

 private:
  fsst_common::FsstBuilt b_;
  std::unique_ptr<Encoder> encoder_;
};

void* build(const lb_chunk_view* view, const char* /*config*/, char* eb, uint64_t ec) {
  auto child = std::make_unique<FsstLikeTum>();
  return lb::adapter_build(
      std::make_unique<lb::DictMatcher>(std::move(child), /*child_copies_input=*/true),
      view, eb, ec);
}

const lb_strategy kStrategies[] = {
    {"dict+interp", LB_OP_BIT(LB_PREFIX) | LB_OP_BIT(LB_SUFFIX) |
                        LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_MULTI_CONTAINS)}};

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"dict_fsst_like_tum",
    /*version=*/"0.2.0+b1eb3ab.resident-state",
    /*cpu_features=*/nullptr,
    /*strategies=*/kStrategies,
    /*strategy_count=*/1,
    /*build=*/build,
    /*footprint=*/lb::adapter_footprint,
    /*run=*/lb::adapter_run,
    /*view=*/nullptr,
    /*decode=*/nullptr,
    /*destroy=*/lb::adapter_destroy,
};

}  // namespace

// Default visibility so the CMake localize step (which hides every other FSST /
// FSST-LIKE symbol) can still export this single entry point.
extern "C" __attribute__((visibility("default"))) const lb_candidate*
lb_candidate_dict_fsst_like_tum(void) {
  return &kVtable;
}
