// llm_token_tum — SGTT "LLM token table" compression (token-vldb2026, TUM),
// decode-only. Rows are greedily tokenized against a STATIC 65536-entry token
// table extracted from the OpenAI cl100k vocabulary ("openai100kpatched":
// patched so IDs 0..255 are the identity byte tokens and every token is 1..7
// bytes), and stored as one uint16 token ID per token. There is NO per-chunk
// training: the dictionary is global and fixed, so build() is a pure
// tokenization pass. The harness composes every scanner over decode()'s output
// (the "decode" strategy) — the decode-then-eval peer of lz4/zstd/fsst.
//
// Adapted from vendor/token-vldb2026/src/compressor/TokenizerCompressor.cpp
// (kept verbatim next to this file for reference; (c) 2025 Tobias Schmidt).
// The matching structures and the hot loops are copied 1:1; the deltas are:
//   - per-ROW tokenization (token sequences never span rows) instead of one
//     whole-buffer pass + footer, so canonical row offsets stay exact and the
//     planned compressed-domain prefilter can treat each row independently;
//   - a bounds-safe row tail: the reference loads 8 bytes through
//     `reader - 8 + remaining`, which under-reads before the buffer for short
//     leading rows; here the tail is built by zero-padded memcpy instead
//     (little-endian equivalent of the reference's shifted load), plus an
//     explicit `len <= remaining` guard;
//   - the token table is embedded via `.incbin` (tokens.S.in) because the
//     upstream `#embed` needs GCC 15+/Clang 19+, and the table invariants the
//     encoder relies on are verified once at init (see Dict::init).
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

#include "util/CompilerHints.hpp"
#include "util/LossyHashMap.hpp"
#include "util/MultiHashMap.hpp"

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <memory>
#include <mutex>
#include <vector>

// Embedded by tokens.S (from vendor/token-vldb2026/src/tokens/openai100kpatched).
extern "C" {
extern const uint8_t lb_llm_token_dict[];     // kTokenCount * kDictStride bytes
extern const uint8_t lb_llm_token_lengths[];  // kTokenCount bytes
}

namespace {

constexpr size_t kTokenCount = 65536;   // token IDs are uint16
constexpr size_t kDictStride = 16;      // upstream stores one uint128 per token
constexpr size_t kMaxTokenSize = 7;     // upstream TokenizerCompressor limit
constexpr size_t kTokenIdBytes = sizeof(uint16_t);

// One 4..7-byte token candidate in the first-4-bytes-keyed multimap
// (identical to the reference's ShortTokenEntry).
struct ShortTokenEntry {
  uint64_t text;
  uint64_t mask;
  uint32_t id;
  uint32_t length;
};
static_assert(sizeof(ShortTokenEntry) == 24);

// The static global dictionary: decode arrays + the reference's three-tier
// encoder index (exact 2-byte array, lossy 3-byte map, 4..7-byte multimap).
// Mirrors TokenizerCompressor's Dict::init 1:1, with the upstream throw
// replaced by an error string (checked by build()) and the two table
// invariants the encoder RELIES on verified explicitly:
//   - IDs 0..255 are identity single-byte tokens (1-byte fallback correctness)
//   - no multi-byte token contains 0x00 (zero-padded tail-match safety)
struct Dict {
  std::vector<uint8_t> lengths;
  std::vector<uint64_t> tokens;

  std::vector<uint16_t> shortTokenArray2;                 // <=2-byte lookup
  sgtt::lossy_hash_map<uint32_t, uint32_t> tokenMap3;     // 3-byte lookup
  sgtt::multi_hash_map<uint32_t, ShortTokenEntry> shortTokenMap;  // 4..7

  const char* error = nullptr;  // non-null => table invariant violated

  void init() {
    lengths.resize(kTokenCount);
    tokens.resize(kTokenCount);
    for (size_t i = 0; i < kTokenCount; ++i) {
      const uint8_t len = lb_llm_token_lengths[i];
      if (len == 0 || len > kMaxTokenSize) {
        error = "invalid token length in openai100kpatched table";
        return;
      }
      lengths[i] = len;
      tokens[i] = unalignedLoad<uint64_t>(lb_llm_token_dict + i * kDictStride);
      if (i < 256 && !(len == 1 && static_cast<uint8_t>(tokens[i]) == i)) {
        error = "token IDs 0..255 are not identity byte tokens";
        return;
      }
      for (size_t b = 0; len > 1 && b < len; ++b) {
        if (lb_llm_token_dict[i * kDictStride + b] == 0) {
          error = "multi-byte token contains 0x00";
          return;
        }
      }
    }

    // 2-byte array (fallback to 1-byte identity tokens).
    shortTokenArray2.resize(65536);
    for (size_t i = 0; i < shortTokenArray2.size(); ++i) {
      shortTokenArray2[i] = static_cast<uint16_t>(i % 256);
    }
    for (uint32_t id = 0; id < kTokenCount; ++id) {
      if (lengths[id] == 2) {
        shortTokenArray2[static_cast<uint16_t>(tokens[id])] =
            static_cast<uint16_t>(id);
      }
    }

    // Lossy map for 3-byte tokens (collisions drop tokens, never correctness).
    {
      std::vector<uint32_t> keys;
      keys.reserve(kTokenCount);
      for (uint32_t id = 0; id < kTokenCount; ++id) {
        if (lengths[id] != 3) continue;
        keys.push_back(static_cast<uint32_t>(tokens[id]) << 8);
      }
      tokenMap3.initialize(keys);
      for (uint32_t id = 0; id < kTokenCount; ++id) {
        if (lengths[id] != 3) continue;
        tokenMap3[static_cast<uint32_t>(tokens[id]) << 8] = id;
      }
    }

    // First-4-bytes-keyed multimap for 4..7-byte tokens.
    {
      std::vector<uint32_t> keys;
      keys.reserve(kTokenCount);
      for (uint32_t id = 0; id < kTokenCount; ++id) {
        const uint8_t len = lengths[id];
        if (len < 4 || len > kMaxTokenSize) continue;
        keys.push_back(static_cast<uint32_t>(tokens[id]));
      }
      shortTokenMap.initialize(keys);
      for (uint32_t id = kTokenCount; id-- > 0;) {
        const uint8_t len = lengths[id];
        if (len < 4 || len > kMaxTokenSize) continue;
        const uint64_t mask = (~static_cast<uint64_t>(0) >> ((8 - len) * 8));
        const uint64_t text = tokens[id] & mask;
        const uint32_t key = static_cast<uint32_t>(tokens[id]);
        shortTokenMap[key] = ShortTokenEntry{
            .text = text, .mask = mask, .id = id, .length = len};
      }
      shortTokenMap.setDefault(
          ShortTokenEntry{.text = 0, .mask = 0xFFull, .id = 0, .length = 1});
    }
  }
};

const Dict& dict() {
  static Dict d;
  static std::once_flag once;
  std::call_once(once, [] { d.init(); });
  return d;
}

// Greedy longest-match against 8 zero-high-padded haystack bytes; identical
// control flow to the reference's match() lambda. Returns (id, length).
forceinline std::pair<uint32_t, uint32_t> MatchToken(const Dict& d,
                                                     uint64_t text) {
  uint32_t id = d.shortTokenArray2[static_cast<uint16_t>(text)];
  uint32_t len = 1 + static_cast<int>(id > 255);

  {
    const uint32_t prefix = static_cast<uint32_t>(text);
    auto [begin, end] = d.shortTokenMap.find(prefix);

    const auto& e1 = *(begin + 0);
    const auto& e2 = *(begin + 1);
    const auto& e3 = *(begin + 2);
    const auto& e4 = *(begin + 3);

    if ((text & e1.mask) == e1.text) return {e1.id, e1.length};
    if ((text & e2.mask) == e2.text) return {e2.id, e2.length};
    if ((text & e3.mask) == e3.text) return {e3.id, e3.length};
    if ((text & e4.mask) == e4.text) return {e4.id, e4.length};

    for (begin = begin + 4; begin < end; ++begin) {
      const auto& e = *begin;
      if ((text & e.mask) == e.text) return {e.id, e.length};
    }
  }

  {
    const uint32_t key = (static_cast<uint32_t>(text) << 8);
    auto it = d.tokenMap3.find(key);
    if (it != d.tokenMap3.end()) return {it->second, 3};
  }

  return {id, len};
}

// Tokenize one row. The main loop is the reference's; the tail differs (see
// file header): zero-padded load + explicit remaining-length guard instead of
// an under-reading shifted load.
void EncodeRow(const Dict& d, const uint8_t* row, size_t row_len,
               std::vector<uint16_t>& out) {
  size_t pos = 0;
  while (pos + 8 <= row_len) {
    const uint64_t text = unalignedLoad<uint64_t>(row + pos);
    const auto [id, len] = MatchToken(d, text);
    out.push_back(static_cast<uint16_t>(id));
    pos += len;
  }
  while (pos < row_len) {
    const size_t remaining = row_len - pos;
    if (remaining == 1) {
      out.push_back(static_cast<uint16_t>(row[pos]));  // 1-byte identity token
      pos += 1;
      continue;
    }
    uint64_t text = 0;
    std::memcpy(&text, row + pos, remaining);  // little-endian zero-pad
    auto [id, len] = MatchToken(d, text);
    if (len > remaining) {  // unreachable while table has no NUL tokens
      id = row[pos];
      len = 1;
    }
    out.push_back(static_cast<uint16_t>(id));
    pos += len;
  }
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
  const Dict& d = dict();
  if (d.error != nullptr) return fail(d.error);

  auto h = std::make_unique<TokBuilt>();
  h->num_rows = view->num_rows;
  h->payload_bytes = view->offsets[view->num_rows];
  h->offsets.assign(view->offsets, view->offsets + view->num_rows + 1);
  // >=2 payload bytes per token, so payload/2 is a lower bound on capacity.
  h->token_ids.reserve(h->payload_bytes / 2 + view->num_rows);
  for (uint64_t i = 0; i < view->num_rows; ++i) {
    EncodeRow(d, view->bytes + view->offsets[i],
              size_t(view->offsets[i + 1] - view->offsets[i]), h->token_ids);
  }
  return h.release();
}

uint32_t cand_footprint(void* self, lb_footprint_component* out,
                        uint32_t capacity) {
  auto* h = static_cast<TokBuilt*>(self);
  const lb_footprint_component components[] = {
      {"payload_tokens", h->token_ids.size() * kTokenIdBytes},
      {"token_table", kTokenCount * (sizeof(uint64_t) + sizeof(uint8_t))},
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

  const Dict& d = dict();
  const uint8_t* lengths = d.lengths.data();
  const uint64_t* tokens = d.tokens.data();

  const uint16_t* reader = h->token_ids.data();
  const uint16_t* limit = reader + h->token_ids.size();
  uint8_t* writer = bytes_out;

  // Reference decompress loop: fixed 8-byte store, advance by true length;
  // the <=7-byte overshoot past the payload is covered by LB_DECODE_PAD.
  for (; reader + 4 <= limit; reader += 4) {
    const auto t0 = reader[0], t1 = reader[1], t2 = reader[2], t3 = reader[3];
    unalignedStore(writer, tokens[t0]);
    writer += lengths[t0];
    unalignedStore(writer, tokens[t1]);
    writer += lengths[t1];
    unalignedStore(writer, tokens[t2]);
    writer += lengths[t2];
    unalignedStore(writer, tokens[t3]);
    writer += lengths[t3];
  }
  for (; reader < limit; ++reader) {
    const auto t = reader[0];
    unalignedStore(writer, tokens[t]);
    writer += lengths[t];
  }

  if (uint64_t(writer - bytes_out) != h->payload_bytes) return 2;
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
