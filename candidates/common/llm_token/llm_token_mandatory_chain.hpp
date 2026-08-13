#pragma once
// =============================================================================
// llm_token_mandatory_chain.hpp — the needle's mandatory token-ID chain.
// =============================================================================
//
// The token-domain analog of fsst_common/fsst_mandatory_chain.hpp (the DuckDB
// algorithm): given the static token table and a needle, return the longest
// run of token IDs that MUST appear — consecutively, each under a single
// fixed ID — in the token stream of ANY row containing the needle, regardless
// of surrounding context. Token IDs are contiguous uint16s in the stored
// stream, so the first 1/2 chain IDs form a 2/4-byte code the byte-domain
// prefilter (prefilter.hpp) can scan for directly. An empty chain means "no
// usable filter" (caller degrades to pass-through), never a false rejection.
//
// The DAG is TIGHTER than FSST's because this encoder is a pure function of
// its static table: at needle offset `o` with r = needle_size - o bytes
// remaining, MatchToken sees an 8-byte window of which only bytes 0..6 are
// ever examined (tokens are <= 7 bytes), so
//
//   r >= 7  : the window is entirely inside the needle — the encoder's
//             decision is DETERMINISTIC. Exactly one edge: o -> o + len.
//   r <  7  : two kinds of edges:
//             - the "definite" pick MatchToken(zero-padded window): by the
//               no-NUL-in-multi-byte-tokens table invariant this is exactly
//               the token the encoder picks whenever no longer,
//               context-dependent token matches (and it never overshoots r);
//             - a conservative edge to accept if ANY token longer than r
//               starts with the remaining r needle bytes (the encoder might
//               emit it under favorable context, consuming past the needle
//               end). Possibly-impossible accept edges only ever SHORTEN the
//               chain — conservative, never a false rejection.
//   node 0  : additionally, the first token of the overlap may have started
//             BEFORE the needle (absorbed left context): for every token, a
//             proper suffix equal to a needle prefix adds 0 -> suffix_len,
//             and the whole needle strictly inside a token adds 0 -> accept
//             (same start-edge construction as upstream FSST's).
//
// Mandatory nodes (offsets every 0->accept path must visit) and the longest
// single-ID run between consecutive mandatory nodes are then extracted
// exactly as in the FSST port.

#include "llm_token_table.hpp"

#include <cstddef>
#include <cstdint>
#include <vector>

namespace llm_token {

namespace chain_detail {

struct ChainEdge {
  uint32_t code;  // token ID
  size_t dst;     // needle offset, or needle_size for accept
};

// Low `n` bytes mask (n in 1..7).
forceinline uint64_t LowMask(size_t n) {
  return ~static_cast<uint64_t>(0) >> ((8 - n) * 8);
}

// Does 0 -> accept remain reachable when `avoid` is removed? (Port of
// fsst_mandatory_chain.hpp's ReachesAcceptAvoiding.)
inline bool ReachesAcceptAvoiding(const std::vector<std::vector<ChainEdge>>& edges,
                                  size_t needle_size, size_t avoid) {
  std::vector<bool> reached(needle_size + 1, false);
  reached[0] = true;
  for (size_t node = 0; node < needle_size; node++) {
    if (!reached[node] || node == avoid) continue;
    for (const auto& edge : edges[node]) reached[edge.dst] = true;
  }
  return reached[needle_size];
}

}  // namespace chain_detail

inline std::vector<uint16_t> LlmTokenMandatoryChain(const TokenDict& d,
                                                    const uint8_t* needle,
                                                    size_t needle_size) {
  using namespace chain_detail;
  if (needle_size == 0) return {};
  const size_t n = needle_size;

  std::vector<std::vector<ChainEdge>> edges(n + 1);

  // Tiling edges: the deterministic encoder step at each offset, plus a
  // conservative accept edge where a token could consume past the needle end.
  for (size_t o = 0; o < n; o++) {
    const size_t r = n - o;
    const uint64_t window = LoadWindow(needle + o, r);
    const auto [id, len] = MatchToken(d, window);
    edges[o].push_back({id, o + len});  // len <= r (no-NUL invariant)

    if (r < kMaxTokenSize) {
      const uint64_t suffix = window & LowMask(r);
      for (uint32_t t = 0; t < kTokenCount; t++) {
        if (d.lengths[t] > r && (d.tokens[t] & LowMask(r)) == suffix) {
          // One accept edge is enough: reachability and the single-ID test
          // react to its presence, not its multiplicity.
          edges[o].push_back({t, n});
          break;
        }
      }
    }
  }

  // Start edges: the first overlap token may have begun before the needle.
  {
    const uint64_t needle_head = LoadWindow(needle, n);
    for (uint32_t t = 0; t < kTokenCount; t++) {
      const size_t len = d.lengths[t];
      for (size_t start = 1; start < len; start++) {
        const size_t suffix_len = len - start;
        if (suffix_len > n) continue;
        const uint64_t tok_suffix = (d.tokens[t] >> (8 * start)) & LowMask(suffix_len);
        if (tok_suffix == (needle_head & LowMask(suffix_len))) {
          edges[0].push_back({t, suffix_len});
        }
      }
      // Needle strictly inside the token (possible only when n <= len - 2).
      for (size_t pos = 1; pos + n < len; pos++) {
        if (((d.tokens[t] >> (8 * pos)) & LowMask(n)) == needle_head) {
          edges[0].push_back({t, n});
          break;
        }
      }
    }
  }

  // Mandatory nodes: offsets no 0->accept path can avoid. (Every node is
  // "alive" here — the 1-byte identity tokens tile any suffix — so upstream's
  // dead-edge pruning pass is unnecessary.)
  std::vector<size_t> mandatory;
  mandatory.push_back(0);
  for (size_t node = 1; node < n; node++) {
    if (!ReachesAcceptAvoiding(edges, n, node)) mandatory.push_back(node);
  }
  mandatory.push_back(n);

  // Longest run of segments each covered by a single fixed token ID.
  std::vector<uint16_t> best;
  std::vector<uint16_t> current;
  for (size_t i = 0; i + 1 < mandatory.size(); i++) {
    const auto& out = edges[mandatory[i]];
    bool single_code = !out.empty();
    for (const auto& edge : out) {
      if (edge.dst != mandatory[i + 1] || edge.code != out[0].code) {
        single_code = false;
        break;
      }
    }
    if (single_code) {
      current.push_back(static_cast<uint16_t>(out[0].code));
      continue;
    }
    if (current.size() > best.size()) best = current;
    current.clear();
  }
  if (current.size() > best.size()) best = std::move(current);
  return best;
}

}  // namespace llm_token
