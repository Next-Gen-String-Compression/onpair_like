#pragma once
// =============================================================================
// fsst_mandatory_chain.hpp — the needle's mandatory FSST-code chain.
// =============================================================================
//
// Ported from DuckDB's fsst_mandatory_chain.cpp (candidates/common/fsst_common/
// fsst_mandatory_chain.cpp is the original) off the DuckDB types (idx_t,
// duckdb::vector, duckdb_fsst_decoder_t) onto std types + a raw cwida decoder.
// The algorithm is unchanged; see the .cpp for the full commentary.
//
// Given an FSST symbol table and a needle, returns the longest run of FSST
// codes that MUST appear — consecutively, under a single fixed code each — in
// the compressed byte stream of ANY row containing the needle, regardless of
// the surrounding context. Those codes are contiguous bytes in the compressed
// stream, so the first 2/4 of them form a 2/4-byte code the byte-domain
// prefilter can scan for directly on the compressed data (no decompression).
//
// An unencodable needle yields an empty chain (caller degrades to pass-through),
// never a false rejection. Assumes greedy longest-match FSST encoding.

#include <cstdint>
#include <cstring>
#include <vector>

namespace fsst_common {

namespace mandatory_detail {

struct FSSTSymbol {
  unsigned char bytes[8];
  uint8_t length;  // symbol byte-length (1-8), 0 for unused codes
};

struct ChainEdge {
  uint8_t code;
  size_t dst;
};

inline bool SymbolMatchesAt(const FSSTSymbol &sym, const unsigned char *needle, size_t needle_size, size_t offset) {
  const size_t remaining = needle_size - offset;
  const size_t common = sym.length < remaining ? sym.length : remaining;
  return std::memcmp(sym.bytes, needle + offset, common) == 0;
}

inline void AddTilingEdges(std::vector<std::vector<ChainEdge>> &edges, const FSSTSymbol *symbols,
                           const unsigned char *needle, size_t needle_size, size_t offset) {
  const size_t remaining = needle_size - offset;
  size_t best_fit_len = 0;
  uint8_t best_fit_code = 0;
  for (uint16_t code = 0; code < 255; code++) {
    const auto &sym = symbols[code];
    if (sym.length == 0 || !SymbolMatchesAt(sym, needle, needle_size, offset)) {
      continue;
    }
    if (sym.length > remaining) {
      edges[offset].push_back({static_cast<uint8_t>(code), needle_size});
    } else if (sym.length > best_fit_len) {
      best_fit_len = sym.length;
      best_fit_code = static_cast<uint8_t>(code);
    }
  }
  if (best_fit_len > 0) {
    edges[offset].push_back({best_fit_code, offset + best_fit_len});
  }
}

inline void AddStartEdges(std::vector<std::vector<ChainEdge>> &edges, const FSSTSymbol *symbols,
                          const unsigned char *needle, size_t needle_size) {
  for (uint16_t code = 0; code < 255; code++) {
    const auto &sym = symbols[code];
    for (size_t start = 1; start < sym.length; start++) {
      const size_t suffix_len = sym.length - start;
      if (suffix_len <= needle_size && std::memcmp(sym.bytes + start, needle, suffix_len) == 0) {
        edges[0].push_back({static_cast<uint8_t>(code), suffix_len});
      }
    }
    for (size_t pos = 1; pos + needle_size < sym.length; pos++) {
      if (std::memcmp(sym.bytes + pos, needle, needle_size) == 0) {
        edges[0].push_back({static_cast<uint8_t>(code), needle_size});
        break;
      }
    }
  }
}

inline bool ReachesAcceptAvoiding(const std::vector<std::vector<ChainEdge>> &edges, size_t needle_size, size_t avoid) {
  std::vector<bool> reached(needle_size + 1, false);
  reached[0] = true;
  for (size_t node = 0; node < needle_size; node++) {
    if (!reached[node] || node == avoid) {
      continue;
    }
    for (const auto &edge : edges[node]) {
      reached[edge.dst] = true;
    }
  }
  return reached[needle_size];
}

}  // namespace mandatory_detail

// lens: decoder.len[255]; symbols_le: decoder.symbol[255] (little-endian u64).
inline std::vector<uint8_t> FsstMandatoryChain(const uint8_t *lens, const uint64_t *symbols_le,
                                               const uint8_t *needle_p, size_t needle_size) {
  using namespace mandatory_detail;
  if (!lens || !symbols_le || needle_size == 0) {
    return {};
  }
  const auto needle = reinterpret_cast<const unsigned char *>(needle_p);

  FSSTSymbol symbols[255];
  for (uint16_t code = 0; code < 255; code++) {
    const size_t length = lens[code];
    if (length < 1 || length > 8) {
      symbols[code].length = 0;
      continue;
    }
    symbols[code].length = static_cast<uint8_t>(length);
    std::memcpy(symbols[code].bytes, &symbols_le[code], 8);
  }

  std::vector<std::vector<ChainEdge>> edges(needle_size + 1);
  for (size_t node = 0; node < needle_size; node++) {
    AddTilingEdges(edges, symbols, needle, needle_size, node);
  }
  AddStartEdges(edges, symbols, needle, needle_size);

  std::vector<bool> alive(needle_size + 1, false);
  alive[needle_size] = true;
  for (size_t node = needle_size; node-- > 0;) {
    auto &out = edges[node];
    size_t kept = 0;
    for (const auto &edge : out) {
      if (alive[edge.dst]) {
        out[kept++] = edge;
      }
    }
    out.resize(kept);
    alive[node] = kept > 0;
  }
  if (!alive[0]) {
    return {};
  }

  std::vector<size_t> mandatory;
  mandatory.push_back(0);
  for (size_t node = 1; node < needle_size; node++) {
    if (!ReachesAcceptAvoiding(edges, needle_size, node)) {
      mandatory.push_back(node);
    }
  }
  mandatory.push_back(needle_size);

  std::vector<uint8_t> best;
  std::vector<uint8_t> current;
  for (size_t i = 0; i + 1 < mandatory.size(); i++) {
    const auto &out = edges[mandatory[i]];
    bool single_code = !out.empty();
    for (const auto &edge : out) {
      if (edge.dst != mandatory[i + 1] || edge.code != out[0].code) {
        single_code = false;
        break;
      }
    }
    if (single_code) {
      current.push_back(out[0].code);
      continue;
    }
    if (current.size() > best.size()) {
      best = current;
    }
    current.clear();
  }
  if (current.size() > best.size()) {
    best = std::move(current);
  }
  return best;
}

}  // namespace fsst_common
