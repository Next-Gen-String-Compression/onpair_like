// llm_token_tum — SGTT "LLM token table" compression (token-vldb2026, TUM),
// decode-only. Rows are greedily tokenized against a STATIC 65536-entry token
// table extracted from the OpenAI cl100k vocabulary ("openai100kpatched":
// patched so IDs 0..255 are the identity byte tokens and every token is 1..7
// bytes), and stored as one uint16 token ID per token. There is NO per-chunk
// training: the dictionary is global and fixed, so build() is a pure
// tokenization pass. The harness composes every scanner over decode()'s output
// (the "decode" strategy) — the decode-then-eval peer of lz4/zstd/fsst. The
// compressed-domain prefilter over the same stored form is llm_token_prefilter.
//
// The tokenizer core (TokenDict/MatchToken/EncodeRow/DecodeTokens, adapted
// from the reference TokenizerCompressor with documented deltas — per-row
// tokenization, bounds-safe tail, .incbin table embedding, init-time invariant
// checks) is the shared candidates/common/llm_token/llm_token_table.hpp; this
// file adds only the chunk build/decode plumbing + the vtable, mirroring how
// the `fsst` candidate sits on fsst_common.
//
// Stored form (footprint components):
//   - payload_tokens : concatenated uint16 token IDs, 2 B per token
//   - token_table    : the resident decode-side table (65536 x (8 B text +
//                      1 B length)). It is a STATIC dictionary shared by every
//                      chunk and every column — reported per chunk for
//                      honesty, amortized in practice (codecs.toml runs one
//                      chunk per column).
//   - offsets        : canonical row index, (rows+1) x u64, uncompressed
//
// Token ID streams are concatenable (decode is context-free per token), so
// decode() is one bulk pass over the whole stream; row boundaries are the
// retained canonical offsets. The fixed 8-byte store per token over-writes at
// most 7 bytes past the payload, covered by LB_DECODE_PAD (SEMANTICS.md §8).

#include "lb_candidate.h"

#include "llm_token_table.hpp"

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <memory>
#include <mutex>
#include <vector>

// Embedded by tokens.S (candidates/common/llm_token/tokens.S.in, configured
// with this candidate's symbol prefix).
extern "C" {
extern const uint8_t lb_llm_token_tum_dict[];     // kTokenCount * kDictStride
extern const uint8_t lb_llm_token_tum_lengths[];  // kTokenCount
}

namespace {

using llm_token::TokenDict;

const TokenDict& dict() {
  static TokenDict d;
  static std::once_flag once;
  std::call_once(once,
                 [] { d.init(lb_llm_token_tum_dict, lb_llm_token_tum_lengths); });
  return d;
}

// One built chunk: the concatenated token stream + retained canonical offsets.
struct TokBuilt {
  std::vector<uint16_t> token_ids;   // concatenated per-row token streams
  std::vector<uint64_t> offsets;     // num_rows + 1 canonical row offsets
  uint64_t num_rows = 0;
  uint64_t payload_bytes = 0;
};

void* cand_build(const lb_chunk_view* view, const char* /*config_json*/,
                 char* err_buf, uint64_t err_cap) {
  auto fail = [&](const char* msg) -> void* {
    if (err_cap > 0) std::snprintf(err_buf, err_cap, "%s", msg);
    return nullptr;
  };
  const TokenDict& d = dict();
  if (d.error != nullptr) return fail(d.error);

  auto h = std::make_unique<TokBuilt>();
  h->num_rows = view->num_rows;
  h->payload_bytes = view->offsets[view->num_rows];
  h->offsets.assign(view->offsets, view->offsets + view->num_rows + 1);
  // >=2 payload bytes per token, so payload/2 is a lower bound on capacity.
  h->token_ids.reserve(h->payload_bytes / 2 + view->num_rows);
  for (uint64_t i = 0; i < view->num_rows; ++i) {
    llm_token::EncodeRow(d, view->bytes + view->offsets[i],
                         size_t(view->offsets[i + 1] - view->offsets[i]),
                         h->token_ids);
  }
  return h.release();
}

uint32_t cand_footprint(void* self, lb_footprint_component* out,
                        uint32_t capacity) {
  auto* h = static_cast<TokBuilt*>(self);
  const lb_footprint_component components[] = {
      {"payload_tokens", h->token_ids.size() * llm_token::kTokenIdBytes},
      {"token_table", llm_token::kDecodeTableBytes},
      {"offsets", (h->num_rows + 1) * sizeof(uint64_t)},
  };
  const uint32_t count = 3;
  for (uint32_t i = 0; i < count && i < capacity; i++) out[i] = components[i];
  return count;
}

int cand_decode(void* self, uint8_t* bytes_out, uint64_t bytes_cap,
                uint64_t* offsets_out) {
  auto* h = static_cast<TokBuilt*>(self);
  if (bytes_cap < h->payload_bytes) return 1;

  const size_t got = llm_token::DecodeTokens(dict(), h->token_ids.data(),
                                             h->token_ids.size(), bytes_out);
  if (got != h->payload_bytes) return 2;
  std::memcpy(offsets_out, h->offsets.data(),
              h->offsets.size() * sizeof(uint64_t));
  return 0;
}

void cand_destroy(void* self) { delete static_cast<TokBuilt*>(self); }

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"llm_token_tum",
    /*version=*/"0.1.0+4b99341",
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

extern "C" const lb_candidate* lb_candidate_llm_token_tum(void) {
  return &kVtable;
}
