#pragma once
// =============================================================================
// uncompressed_matchers.hpp — the two plaintext Matchers.
// =============================================================================
//
//   UncompressedMemmem    — per-row libc memmem() (+ anchored/ordered variants),
//                            the C++ analog of the memchr `memmem` baseline.
//   UncompressedPrefilter  — the code-prefilter: extract a 2/4-byte code from the
//                            tail of the needle, reject rows that cannot hold it
//                            with prefilter.hpp, verify survivors with find().
//
// Both store the column *non-owning* (build retains the caller's pointers); the
// harness view or a parent DictMatcher's unique-value buffers keep them alive.
// Compression ratio 1.0 — footprint is the raw payload + row offsets.

#include "matcher.hpp"
#include "prefilter.hpp"

#include <cstring>
#include <string_view>

namespace lb {

// Shared non-owning column store + footprint for the plaintext matchers.
class UncompressedBase : public Matcher {
 public:
  bool build(const uint8_t* bytes, const uint64_t* offsets, uint64_t n, char*,
             uint64_t) override {
    bytes_ = bytes;
    offsets_ = offsets;
    n_ = n;
    return true;
  }
  void footprint(std::vector<lb_footprint_component>& out) const override {
    out.push_back({"payload", n_ ? offsets_[n_] : 0});
    out.push_back({"offsets", (n_ + 1) * sizeof(uint64_t)});
  }
  uint64_t num_rows() const override { return n_; }

 protected:
  std::string_view row(uint64_t i) const {
    return {reinterpret_cast<const char*>(bytes_) + offsets_[i],
            static_cast<size_t>(offsets_[i + 1] - offsets_[i])};
  }
  static void set_bit(uint64_t* w, uint64_t i) { w[i >> 6] |= uint64_t{1} << (i & 63); }

  const uint8_t* bytes_ = nullptr;
  const uint64_t* offsets_ = nullptr;
  uint64_t n_ = 0;
};

class UncompressedMemmem final : public UncompressedBase {
 public:
  uint32_t supported_ops() const override { return LB_ALL_OPS; }

  int run(const lb_query* q, uint64_t* out, lb_run_stats*) override {
    switch (q->op) {
      case LB_CONTAINS: {
        const auto& nd = q->needles[0];
        for (uint64_t i = 0; i < n_; i++) {
          const std::string_view r = row(i);
          if (contains(r, nd.ptr, nd.len)) set_bit(out, i);
        }
        return 0;
      }
      case LB_PREFIX: {
        const auto& nd = q->needles[0];
        for (uint64_t i = 0; i < n_; i++) {
          const std::string_view r = row(i);
          if (r.size() >= nd.len &&
              std::memcmp(r.data(), nd.ptr, nd.len) == 0)
            set_bit(out, i);
        }
        return 0;
      }
      case LB_SUFFIX: {
        const auto& nd = q->needles[0];
        for (uint64_t i = 0; i < n_; i++) {
          const std::string_view r = row(i);
          if (r.size() >= nd.len &&
              std::memcmp(r.data() + r.size() - nd.len, nd.ptr, nd.len) == 0)
            set_bit(out, i);
        }
        return 0;
      }
      case LB_MULTI_CONTAINS: {
        for (uint64_t i = 0; i < n_; i++) {
          const std::string_view r = row(i);
          size_t pos = 0;
          bool ok = true;
          for (uint32_t k = 0; k < q->needle_count; k++) {
            const auto& nd = q->needles[k];
            const void* hit = ::memmem(r.data() + pos, r.size() - pos, nd.ptr, nd.len);
            if (!hit) { ok = false; break; }
            pos = static_cast<const char*>(hit) - r.data() + nd.len;
          }
          if (ok) set_bit(out, i);
        }
        return 0;
      }
      case LB_CONTAINS_ANY: {
        for (uint64_t i = 0; i < n_; i++) {
          const std::string_view r = row(i);
          for (uint32_t k = 0; k < q->needle_count; k++) {
            const auto& nd = q->needles[k];
            if (contains(r, nd.ptr, nd.len)) { set_bit(out, i); break; }
          }
        }
        return 0;
      }
      default:
        return 1;
    }
  }

 private:
  static bool contains(std::string_view r, const uint8_t* n, uint64_t len) {
    return ::memmem(r.data(), r.size(), n, len) != nullptr;  // empty needle -> match
  }
};

class UncompressedPrefilter final : public UncompressedBase {
 public:
  uint32_t supported_ops() const override {
    return LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY);
  }

  int run(const lb_query* q, uint64_t* out, lb_run_stats* stats) override {
    uint64_t candidates = 0;
    if (q->op == LB_CONTAINS) {
      const auto& nd = q->needles[0];
      if (nd.len == 0) {  // '%%' matches everything
        for (uint64_t i = 0; i < n_; i++) set_bit(out, i);
        if (stats) stats->prefilter_candidates = n_;
        return 0;
      }
      const Code code = extract_code(nd.ptr, nd.len);
      const std::string_view needle(reinterpret_cast<const char*>(nd.ptr), nd.len);
      for (uint64_t i = 0; i < n_; i++) {
        const std::string_view r = row(i);
        const bool pf = passes(code, r.data(), static_cast<uint32_t>(r.size()));
        candidates += pf;
        if (pf && r.find(needle) != std::string_view::npos) set_bit(out, i);
      }
    } else if (q->op == LB_CONTAINS_ANY) {
      std::vector<Code> codes(q->needle_count);
      bool match_all = false;
      for (uint32_t k = 0; k < q->needle_count; k++) {
        codes[k] = extract_code(q->needles[k].ptr, q->needles[k].len);
        if (q->needles[k].len == 0) match_all = true;
      }
      if (match_all) {
        for (uint64_t i = 0; i < n_; i++) set_bit(out, i);
        if (stats) stats->prefilter_candidates = n_;
        return 0;
      }
      for (uint64_t i = 0; i < n_; i++) {
        const std::string_view r = row(i);
        for (uint32_t k = 0; k < q->needle_count; k++) {
          if (!passes(codes[k], r.data(), static_cast<uint32_t>(r.size()))) continue;
          candidates++;
          const auto& nd = q->needles[k];
          if (r.find(std::string_view(reinterpret_cast<const char*>(nd.ptr), nd.len)) !=
              std::string_view::npos) {
            set_bit(out, i);
            break;
          }
        }
      }
    } else {
      return 1;
    }
    if (stats) stats->prefilter_candidates = candidates;
    return 0;
  }

 private:
  struct Code {
    uint32_t width;
    uint32_t bytes;
  };
  // Code from the *tail* of the needle: on URLs the leading bytes share common
  // prefixes and filter weakly, so the last 4 (or 2) bytes are more selective.
  static Code extract_code(const uint8_t* n, uint64_t len) {
    if (len >= 4) {
      uint32_t c;
      std::memcpy(&c, n + len - 4, 4);
      return {4, c};
    }
    if (len >= 2) {
      uint16_t c;
      std::memcpy(&c, n + len - 2, 2);
      return {2, static_cast<uint32_t>(c)};
    }
    return {0, 0};
  }
  static bool passes(const Code& c, const char* row, uint32_t len) {
    if (c.width == 4) return prefilter::contains<uint32_t>(row, len, c.bytes);
    if (c.width == 2)
      return prefilter::contains<uint16_t>(row, len, static_cast<uint16_t>(c.bytes));
    return true;
  }
};

}  // namespace lb
