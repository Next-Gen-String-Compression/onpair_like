#pragma once
// =============================================================================
// dict_matcher.hpp — the dictionary front-end, an encoded-domain Matcher.
// =============================================================================
//
// A DictMatcher wraps a *child* Matcher. build() deduplicates the column into
// the set of unique row values (a contiguous blob + offsets) plus one code per
// row, then builds the child over the UNIQUE values only. run() evaluates the
// child once per unique value into a small unique-bitmap, then scatters that
// result to the row bitmap through the codes.
//
// This is the whole point of dictionary encoding for scans: a `LIKE '%x%'`
// predicate is answered n_unique times, not n_rows times. With heavy value
// repetition (URLs, categoricals) n_unique << n_rows, so the child's per-value
// cost is amortized away. The child can be anything — plaintext (memmem /
// prefilter) or a compressed-domain engine (FSST-LIKE) — so `dict_<child>`
// pipelines compose by construction.
//
// Footprint = the child's footprint over the unique dictionary + the per-row
// codes, bit-packed to ceil(log2(n_unique)) bits/row (the representative
// resident cost of a dictionary column; the reference DictionaryAlgorithm).

#include "matcher.hpp"

#include <cstdint>
#include <string_view>
#include <unordered_map>
#include <vector>

namespace lb {

class DictMatcher final : public Matcher {
 public:
  explicit DictMatcher(std::unique_ptr<Matcher> child) : child_(std::move(child)) {}

  bool build(const uint8_t* bytes, const uint64_t* offsets, uint64_t n,
             char* err_buf, uint64_t err_cap) override {
    n_ = n;
    codes_.resize(n);
    unique_offsets_.clear();
    unique_offsets_.push_back(0);
    unique_blob_.clear();

    std::unordered_map<std::string_view, uint32_t> seen;
    seen.reserve(n);
    for (uint64_t i = 0; i < n; i++) {
      const std::string_view v(reinterpret_cast<const char*>(bytes) + offsets[i],
                               static_cast<size_t>(offsets[i + 1] - offsets[i]));
      auto [it, inserted] = seen.try_emplace(v, static_cast<uint32_t>(unique_offsets_.size() - 1));
      if (inserted) {
        unique_blob_.insert(unique_blob_.end(), v.begin(), v.end());
        unique_offsets_.push_back(unique_blob_.size());
      }
      codes_[i] = it->second;
    }
    num_unique_ = static_cast<uint32_t>(unique_offsets_.size() - 1);

    // Build the child over the unique values only. Our buffers outlive the
    // child (both members here), satisfying the non-owning lifetime contract.
    return child_->build(unique_blob_.data(), unique_offsets_.data(), num_unique_,
                         err_buf, err_cap);
  }

  int run(const lb_query* q, uint64_t* out, lb_run_stats* stats) override {
    const size_t uwords = (static_cast<size_t>(num_unique_) + 63) / 64;
    std::vector<uint64_t> ubits(uwords, 0);
    const int rc = child_->run(q, ubits.data(), stats);
    if (rc != 0) return rc;
    // Scatter: row i matches iff its unique value matched.
    for (uint64_t i = 0; i < n_; i++) {
      const uint32_t c = codes_[i];
      if ((ubits[c >> 6] >> (c & 63)) & 1) out[i >> 6] |= uint64_t{1} << (i & 63);
    }
    return 0;
  }

  uint32_t supported_ops() const override { return child_->supported_ops(); }

  void footprint(std::vector<lb_footprint_component>& out) const override {
    // The child's cost is measured over the unique dictionary (its "payload"
    // and "offsets" are unique-domain).
    child_->footprint(out);
    // Per-row codes, bit-packed: ceil(log2(n_unique)) bits each (min 1).
    uint32_t bits = 1;
    while ((uint64_t{1} << bits) < num_unique_) bits++;
    out.push_back({"codes", (n_ * bits + 7) / 8});
  }

  uint64_t num_rows() const override { return n_; }

 private:
  std::unique_ptr<Matcher> child_;
  std::vector<uint8_t> unique_blob_;
  std::vector<uint64_t> unique_offsets_;  // num_unique + 1
  std::vector<uint32_t> codes_;           // one per row
  uint64_t n_ = 0;
  uint32_t num_unique_ = 0;
};

}  // namespace lb
