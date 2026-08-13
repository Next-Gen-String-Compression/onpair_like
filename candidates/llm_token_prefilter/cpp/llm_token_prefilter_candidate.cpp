// llm_token_prefilter — COMPRESSED-DOMAIN prefilter over the SGTT LLM-token
// stream, and its dictionary peer dict_llm_token_prefilter. Two candidates
// from one table copy (this file exports both entry points). The token-domain
// port of fsst_prefilter:
//   1. build()  tokenizes each row against the static openai100kpatched table
//      (candidates/common/llm_token) into uint16 IDs + per-row token offsets.
//   2. per query, translate each needle into its mandatory token-ID chain
//      (llm_token_mandatory_chain.hpp) — the run of IDs that MUST appear in
//      any matching row's token stream — and pack the first 2 (or 1) IDs into
//      a 4/2-byte code.
//   3. scan every row's token bytes for that code with the byte-domain
//      prefilter (prefilter.hpp). Odd-phase (u16-misaligned) byte hits are
//      possible false positives; the verify removes them. No decode on
//      rejected rows.
//   4. decode only the SURVIVORS (per row) and verify the real needle.
//
//   dict_llm_token_prefilter : the same matcher as a DictMatcher child, so
//                              chain building + the compressed scan run once
//                              per UNIQUE value.
//
// vs fsst_prefilter: the chain needs only 2 IDs (not 4 codes) for a 4-byte
// scan target, the table is static (no per-chunk training; chains depend only
// on the needle, though they are still built per run() call — SEMANTICS.md
// forbids cross-call memoization), and rows were tokenized independently at
// build, so per-row token streams are self-contained by construction.
//
// A needle with no usable chain degrades to pass-through (every row a
// survivor); the verify is the correctness authority.

#include "lb_candidate.h"

#include "llm_token_mandatory_chain.hpp"
#include "llm_token_table.hpp"

#include "dict_matcher.hpp"
#include "matcher.hpp"
#include "prefilter.hpp"

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <memory>
#include <mutex>
#include <string_view>
#include <vector>

// Embedded by tokens.S (candidates/common/llm_token/tokens.S.in, configured
// with this candidate's symbol prefix).
extern "C" {
extern const uint8_t lb_llm_token_pf_dict[];     // kTokenCount * kDictStride
extern const uint8_t lb_llm_token_pf_lengths[];  // kTokenCount
}

namespace {

using llm_token::TokenDict;

const TokenDict& dict() {
  static TokenDict d;
  static std::once_flag once;
  std::call_once(once,
                 [] { d.init(lb_llm_token_pf_dict, lb_llm_token_pf_lengths); });
  return d;
}

class LlmTokenPrefilter final : public lb::Matcher {
 public:
  bool build(const uint8_t* bytes, const uint64_t* offsets, uint64_t n,
             char* eb, uint64_t ec) override {
    const TokenDict& d = dict();
    if (d.error != nullptr) {
      if (ec > 0) std::snprintf(eb, ec, "%s", d.error);
      return false;
    }
    n_ = n;
    payload_bytes_ = offsets[n];
    ids_.clear();
    ids_.reserve(payload_bytes_ / 2 + n);
    toffs_.resize(n + 1);
    toffs_[0] = 0;
    uint64_t max_row = 0;
    for (uint64_t i = 0; i < n; i++) {
      const uint64_t len = offsets[i + 1] - offsets[i];
      if (len > max_row) max_row = len;
      llm_token::EncodeRow(d, bytes + offsets[i], size_t(len), ids_);
      toffs_[i + 1] = ids_.size();
    }
    // Per-row decode scratch (survivors only): the longest decoded row + pad.
    rowbuf_.resize(max_row + LB_DECODE_PAD);
    return true;
  }

  int run(const lb_query* q, uint64_t* out, lb_run_stats* stats) override {
    if (q->op != LB_CONTAINS && q->op != LB_CONTAINS_ANY) return 1;

    // Empty needle among the set => '%%' matches every row.
    for (uint32_t k = 0; k < q->needle_count; k++) {
      if (q->needles[k].len == 0) {
        for (uint64_t i = 0; i < n_; i++) out[i >> 6] |= uint64_t{1} << (i & 63);
        if (stats) stats->prefilter_candidates = n_;
        return 0;
      }
    }

    // Per-needle mandatory-chain scan targets. One width for the whole set:
    // the shortest chain picks it (2 IDs -> u32 scan, 1 ID -> u16 scan, 0 ->
    // pass-through). Chain IDs are consecutive u16s in the stored stream.
    const TokenDict& d = dict();
    std::vector<uint32_t> t32;
    std::vector<uint16_t> t16;
    size_t min_chain = SIZE_MAX;
    std::vector<std::vector<uint16_t>> chains;
    for (uint32_t k = 0; k < q->needle_count; k++) {
      chains.push_back(llm_token::LlmTokenMandatoryChain(
          d, q->needles[k].ptr, static_cast<size_t>(q->needles[k].len)));
      if (chains.back().size() < min_chain) min_chain = chains.back().size();
    }
    if (min_chain >= 2) {
      for (auto& c : chains) {
        uint32_t t;
        std::memcpy(&t, c.data(), 4);
        t32.push_back(t);
      }
    } else if (min_chain >= 1) {
      for (auto& c : chains) t16.push_back(c[0]);
    }
    const bool passthrough = t32.empty() && t16.empty();

    uint64_t candidates = 0;
    for (uint64_t i = 0; i < n_; i++) {
      const char* cp = reinterpret_cast<const char*>(ids_.data() + toffs_[i]);
      const uint32_t clen =
          static_cast<uint32_t>((toffs_[i + 1] - toffs_[i]) * sizeof(uint16_t));
      // Compressed-domain prefilter: does this row's ID stream contain any
      // target (at any byte phase)?
      bool pf = passthrough;
      if (!pf) {
        if (!t32.empty()) {
          for (uint32_t c : t32)
            if (prefilter::contains<uint32_t>(cp, clen, c)) { pf = true; break; }
        } else {
          for (uint16_t c : t16)
            if (prefilter::contains<uint16_t>(cp, clen, c)) { pf = true; break; }
        }
      }
      if (!pf) continue;
      candidates++;
      // Verify: decode this one row, then exact substring check.
      const size_t declen = llm_token::DecodeTokens(
          d, ids_.data() + toffs_[i], size_t(toffs_[i + 1] - toffs_[i]),
          rowbuf_.data());
      const std::string_view row(reinterpret_cast<const char*>(rowbuf_.data()),
                                 declen);
      bool hit = false;
      for (uint32_t k = 0; k < q->needle_count && !hit; k++) {
        const std::string_view needle(
            reinterpret_cast<const char*>(q->needles[k].ptr),
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
    // `offsets` is the token-stream row index — the stored form's only row
    // index (canonical byte offsets are not retained). Same 3-component shape
    // as llm_token_tum for comparability; token_table is the shared static
    // dictionary (see llm_token_tum's header on per-chunk attribution).
    out.push_back({"payload_tokens", ids_.size() * llm_token::kTokenIdBytes});
    out.push_back({"token_table", llm_token::kDecodeTableBytes});
    out.push_back({"offsets", (n_ + 1) * sizeof(uint64_t)});
  }

  uint64_t num_rows() const override { return n_; }

 private:
  std::vector<uint16_t> ids_;     // concatenated per-row token streams
  std::vector<uint64_t> toffs_;   // n + 1 row offsets, in TOKENS
  std::vector<uint8_t> rowbuf_;   // survivor decode scratch
  uint64_t n_ = 0;
  uint64_t payload_bytes_ = 0;
};

void* build_plain(const lb_chunk_view* view, const char*, char* eb, uint64_t ec) {
  return lb::adapter_build(std::make_unique<LlmTokenPrefilter>(), view, eb, ec);
}
void* build_dict(const lb_chunk_view* view, const char*, char* eb, uint64_t ec) {
  auto child = std::make_unique<LlmTokenPrefilter>();
  return lb::adapter_build(std::make_unique<lb::DictMatcher>(std::move(child)),
                           view, eb, ec);
}

const uint32_t kOps = LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY);

const lb_strategy kPlainStrats[] = {{"llm-token-prefilter", kOps}};
const lb_candidate kPlainVtable = {
    LB_ABI_VERSION, "llm_token_prefilter", "0.1.0+4b99341", nullptr,
    kPlainStrats, 1, build_plain, lb::adapter_footprint, lb::adapter_run,
    nullptr, nullptr, lb::adapter_destroy};

const lb_strategy kDictStrats[] = {{"dict+llm-token-prefilter", kOps}};
const lb_candidate kDictVtable = {
    LB_ABI_VERSION, "dict_llm_token_prefilter", "0.1.0+4b99341", nullptr,
    kDictStrats, 1, build_dict, lb::adapter_footprint, lb::adapter_run,
    nullptr, nullptr, lb::adapter_destroy};

}  // namespace

extern "C" __attribute__((visibility("default"))) const lb_candidate*
lb_candidate_llm_token_prefilter(void) {
  return &kPlainVtable;
}
extern "C" __attribute__((visibility("default"))) const lb_candidate*
lb_candidate_dict_llm_token_prefilter(void) {
  return &kDictVtable;
}
