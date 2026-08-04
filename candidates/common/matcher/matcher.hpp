#pragma once
// =============================================================================
// matcher.hpp — the composable candidate core.
// =============================================================================
//
// A `Matcher` is one string column's compressor+evaluator: build() stores an
// internal representation of a chunk, run() answers a single query into a row
// bitmap, footprint() reports the resident cost. It is the unit the dictionary
// front-end (dict_matcher.hpp) composes over: a DictMatcher holds a *child*
// Matcher, deduplicates the column into unique values, and evaluates the child
// once per unique value (the encoded domain) instead of once per row.
//
// Concrete matchers live in their own headers (uncompressed_matchers.hpp,
// dict_matcher.hpp, and per-candidate .cpp files for the heavier engines).
// Each candidate crate is a thin wrapper: instantiate a Matcher (optionally
// wrapped in a DictMatcher) and expose it through the C-ABI adapter below.
//
// Lifetime: build() receives a column as (bytes, offsets, n) and may retain
// those pointers *non-owning* — the caller (the harness view, or a parent
// DictMatcher's unique-value buffers) guarantees they outlive the Matcher.

#include "lb_candidate.h"

#include <cstdint>
#include <memory>
#include <vector>

namespace lb {

class Matcher {
 public:
  virtual ~Matcher() = default;

  // Build over a column: row i occupies bytes[offsets[i] .. offsets[i+1]),
  // offsets has n+1 entries with offsets[0]==0. Returns false on failure,
  // writing a NUL-terminated message into err_buf. The pointers may be
  // retained non-owning (see the lifetime note above).
  virtual bool build(const uint8_t* bytes, const uint64_t* offsets, uint64_t n,
                     char* err_buf, uint64_t err_cap) = 0;

  // Answer one query: set bit i (LSB-first within little-endian u64 words, the
  // slice pre-zeroed) for each matching row. Returns 0 on success. `stats` is
  // NULL in timing mode; when non-NULL the matcher may fill fields it measures.
  virtual int run(const lb_query* query, uint64_t* out_bitmap_words,
                  lb_run_stats* stats_or_null) = 0;

  // Which ops this matcher answers, as an LB_OP_BIT mask.
  virtual uint32_t supported_ops() const = 0;

  // Append resident footprint components (payload/offsets/codes/...).
  virtual void footprint(std::vector<lb_footprint_component>& out) const = 0;

  virtual uint64_t num_rows() const = 0;
};

// --------------------------------------------------------------------------
// C-ABI adapter: bridges a Matcher to the lb_candidate vtable. A candidate
// crate provides build() (instantiating its Matcher) and a static vtable that
// points footprint/run/destroy at these shared functions.
// --------------------------------------------------------------------------

struct CandHandle {
  std::unique_ptr<Matcher> matcher;
};

// Build a Matcher over `view` and wrap it in a CandHandle. Returns NULL on
// failure (message already written to err_buf by the Matcher).
inline void* adapter_build(std::unique_ptr<Matcher> m, const lb_chunk_view* view,
                           char* err_buf, uint64_t err_cap) {
  if (!m->build(view->bytes, view->offsets, view->num_rows, err_buf, err_cap)) {
    return nullptr;
  }
  return new CandHandle{std::move(m)};
}

inline uint32_t adapter_footprint(void* self, lb_footprint_component* out,
                                  uint32_t capacity) {
  std::vector<lb_footprint_component> comps;
  static_cast<CandHandle*>(self)->matcher->footprint(comps);
  for (uint32_t i = 0; i < comps.size() && i < capacity; i++) out[i] = comps[i];
  return static_cast<uint32_t>(comps.size());
}

inline int adapter_run(void* self, uint32_t strategy_index, const lb_query* query,
                       uint64_t* out_bitmap_words, lb_run_stats* stats_or_null) {
  if (strategy_index != 0) return 10;
  return static_cast<CandHandle*>(self)->matcher->run(query, out_bitmap_words,
                                                      stats_or_null);
}

inline void adapter_destroy(void* self) { delete static_cast<CandHandle*>(self); }

}  // namespace lb
