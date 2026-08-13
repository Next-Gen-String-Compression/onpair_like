#pragma once
// =============================================================================
// llm_token_table.hpp — the shared SGTT tokenizer core (token-vldb2026, TUM).
// =============================================================================
//
// The static-global-token-table encoder/decoder shared by the `llm_token_tum`
// (decode-only codec) and `llm_token_prefilter` (compressed-domain prefilter)
// candidates. Adapted from vendor/token-vldb2026/src/compressor/
// TokenizerCompressor.cpp (kept verbatim in ./vendor for reference; (c) 2025
// Tobias Schmidt): the matching structures and hot loops are copied 1:1; the
// deltas are per-ROW tokenization, a bounds-safe zero-padded row tail, and
// init-time verification of the table invariants the encoder relies on (see
// TokenDict::init). Each candidate embeds its own copy of the token table via
// `.incbin` (../common/llm_token/tokens.S.in, symbol-prefixed per candidate)
// and passes the two raw arrays to TokenDict::init — this header names no
// global symbols, so the candidates stay independently linkable.
//
// Everything here has internal-or-inline linkage; include it from exactly one
// TU per candidate.

#include "util/CompilerHints.hpp"
#include "util/LossyHashMap.hpp"
#include "util/MultiHashMap.hpp"

#include <cstdint>
#include <cstring>
#include <utility>
#include <vector>

namespace llm_token {

constexpr size_t kTokenCount = 65536;   // token IDs are uint16
constexpr size_t kDictStride = 16;      // upstream stores one uint128 per token
constexpr size_t kMaxTokenSize = 7;     // upstream TokenizerCompressor limit
constexpr size_t kTokenIdBytes = sizeof(uint16_t);
// Resident decode-side table: 8 B text + 1 B length per token.
constexpr uint64_t kDecodeTableBytes =
    kTokenCount * (sizeof(uint64_t) + sizeof(uint8_t));

// One 4..7-byte token candidate in the first-4-bytes-keyed multimap
// (identical to the reference's ShortTokenEntry).
struct ShortTokenEntry {
  uint64_t text;
  uint64_t mask;
  uint32_t id;
  uint32_t length;
};
static_assert(sizeof(ShortTokenEntry) == 24);

// Zero-padded little-endian load of the first min(8, remaining) bytes: the
// exact equivalent of the reference's shifted tail load, without its
// before-the-buffer under-read. Only bytes 0..6 of the result are ever
// examined by MatchToken (tokens are <= 7 bytes), so 8+ remaining bytes give
// the same match decisions as a direct 8-byte load.
forceinline uint64_t LoadWindow(const uint8_t* p, size_t remaining) {
  if (remaining >= 8) return unalignedLoad<uint64_t>(p);
  uint64_t text = 0;
  std::memcpy(&text, p, remaining);
  return text;
}

// The static global dictionary: decode arrays + the reference's three-tier
// encoder index (exact 2-byte array, lossy 3-byte map, 4..7-byte multimap).
// Mirrors TokenizerCompressor's Dict::init 1:1, with the upstream throw
// replaced by an error string (checked by build()) and the two table
// invariants the encoder RELIES on verified explicitly:
//   - IDs 0..255 are identity single-byte tokens (1-byte fallback correctness)
//   - no multi-byte token contains 0x00 (zero-padded tail-match safety)
struct TokenDict {
  std::vector<uint8_t> lengths;
  std::vector<uint64_t> tokens;

  std::vector<uint16_t> shortTokenArray2;                 // <=2-byte lookup
  sgtt::lossy_hash_map<uint32_t, uint32_t> tokenMap3;     // 3-byte lookup
  sgtt::multi_hash_map<uint32_t, ShortTokenEntry> shortTokenMap;  // 4..7

  const char* error = nullptr;  // non-null => table invariant violated

  // dict_bytes: kTokenCount * kDictStride; length_bytes: kTokenCount.
  void init(const uint8_t* dict_bytes, const uint8_t* length_bytes) {
    lengths.resize(kTokenCount);
    tokens.resize(kTokenCount);
    for (size_t i = 0; i < kTokenCount; ++i) {
      const uint8_t len = length_bytes[i];
      if (len == 0 || len > kMaxTokenSize) {
        error = "invalid token length in openai100kpatched table";
        return;
      }
      lengths[i] = len;
      tokens[i] = unalignedLoad<uint64_t>(dict_bytes + i * kDictStride);
      if (i < 256 && !(len == 1 && static_cast<uint8_t>(tokens[i]) == i)) {
        error = "token IDs 0..255 are not identity byte tokens";
        return;
      }
      for (size_t b = 0; len > 1 && b < len; ++b) {
        if (dict_bytes[i * kDictStride + b] == 0) {
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

// Greedy longest-match against 8 zero-high-padded haystack bytes; identical
// control flow to the reference's match() lambda. Returns (id, length). This
// IS the encoder's per-position decision procedure: given the same 8-byte
// window it always returns the same token, which the mandatory-chain builder
// (llm_token_mandatory_chain.hpp) exploits.
forceinline std::pair<uint32_t, uint32_t> MatchToken(const TokenDict& d,
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

// Tokenize one row, appending uint16 token IDs to `out`. The main loop is the
// reference's; the tail is the bounds-safe zero-padded variant (see file
// header) with an explicit remaining-length guard.
inline void EncodeRow(const TokenDict& d, const uint8_t* row, size_t row_len,
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
    auto [id, len] = MatchToken(d, LoadWindow(row + pos, remaining));
    if (len > remaining) {  // unreachable while table has no NUL tokens
      id = row[pos];
      len = 1;
    }
    out.push_back(static_cast<uint16_t>(id));
    pos += len;
  }
}

// Decode `count` token IDs into `writer`, returning the number of bytes
// written. Reference decompress loop: fixed 8-byte store, advance by true
// length — the caller must provide >= 7 bytes of writable slack past the
// decoded payload (LB_DECODE_PAD covers it).
inline size_t DecodeTokens(const TokenDict& d, const uint16_t* reader,
                           size_t count, uint8_t* writer) {
  const uint8_t* lengths = d.lengths.data();
  const uint64_t* tokens = d.tokens.data();
  const uint16_t* limit = reader + count;
  uint8_t* out = writer;

  for (; reader + 4 <= limit; reader += 4) {
    const auto t0 = reader[0], t1 = reader[1], t2 = reader[2], t3 = reader[3];
    unalignedStore(out, tokens[t0]);
    out += lengths[t0];
    unalignedStore(out, tokens[t1]);
    out += lengths[t1];
    unalignedStore(out, tokens[t2]);
    out += lengths[t2];
    unalignedStore(out, tokens[t3]);
    out += lengths[t3];
  }
  for (; reader < limit; ++reader) {
    const auto t = reader[0];
    unalignedStore(out, tokens[t]);
    out += lengths[t];
  }
  return static_cast<size_t>(out - writer);
}

}  // namespace llm_token
