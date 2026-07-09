// onpair — OnPair (github.com/gargiulofrancesco/onpair_cpp, pinned in
// CMakeLists.txt): field-level string compression with random access and
// compressed-domain predicates. Two ways to answer queries:
//
//   - candidate strategy "compressed": token automata driven directly over
//     the bit-packed stream (prefix / contains / contains_any). suffix and
//     multi_contains are not expressible in OnPair's compressed domain and
//     stay unsupported — the decode path covers them.
//   - harness-composed "decode": decompress_all() into harness scratch,
//     then any scanner.
//
// ABI notes: OnPair's offsets are uint32_t (a chunk is capped at 4 GiB of
// payload — build() rejects larger chunks), the contract's are uint64_t;
// build() narrows, decode() widens in place. The decoder over-copies a
// fixed MAX_TOKEN_SIZE per token, covered by the contract's LB_DECODE_PAD
// headroom (SEMANTICS.md rule 8).

#include "lb_candidate.h"

#include <onpair/api.h>

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

namespace {

// ------------------------------------------------------------------ config

// Flat-JSON config reader. The harness guarantees config_json is valid
// JSON; this reader additionally rejects unknown keys and out-of-range
// values so a typo'd config never silently runs defaults. Accepted keys:
//   "bits":            integer in [9, 16] — dictionary = 2^bits entries
//                      (default 16)
//   "threshold":       number in (0.0, 1.0] — DynamicThreshold sample
//                      fraction (default 0.15)
//   "fixed_threshold": integer in [2, 255] — FixedThreshold merge count
//                      (mutually exclusive with "threshold")
//   "seed":            integer — training RNG seed. Defaults to 42, NOT
//                      the library's non-deterministic default: a build
//                      must be reproducible from its recorded config.
struct ParsedConfig {
  onpair::encoding::TrainingConfig cfg;
  std::string error;  // non-empty on failure
};

const char* skip_ws(const char* p) {
  while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') p++;
  return p;
}

ParsedConfig parse_config(const char* json) {
  ParsedConfig out;
  out.cfg.bits = 16;
  out.cfg.threshold = onpair::encoding::DynamicThreshold{0.15};
  out.cfg.seed = 42;

  const char* p = skip_ws(json);
  if (*p != '{') {
    out.error = "config must be a JSON object";
    return out;
  }
  p = skip_ws(p + 1);
  if (*p == '}') return out;
  for (;;) {
    if (*p != '"') {
      out.error = "expected a key";
      return out;
    }
    const char* kend = std::strchr(p + 1, '"');
    if (kend == nullptr) {
      out.error = "unterminated key";
      return out;
    }
    const std::string_view key(p + 1, static_cast<size_t>(kend - p - 1));
    p = skip_ws(kend + 1);
    if (*p != ':') {
      out.error = "expected ':'";
      return out;
    }
    p = skip_ws(p + 1);
    char* vend = nullptr;
    const double v = std::strtod(p, &vend);
    if (vend == p) {
      out.error = "config values must be numbers (key \"" + std::string(key) + "\")";
      return out;
    }
    p = vend;
    if (key == "bits") {
      if (v < 9 || v > 16 || v != double(int(v))) {
        out.error = "\"bits\" must be an integer in [9, 16]";
        return out;
      }
      out.cfg.bits = onpair::BitWidth(v);
    } else if (key == "threshold") {
      if (!(v > 0.0) || v > 1.0) {
        out.error = "\"threshold\" must be in (0.0, 1.0]";
        return out;
      }
      out.cfg.threshold = onpair::encoding::DynamicThreshold{v};
    } else if (key == "fixed_threshold") {
      if (v < 2 || v > 255 || v != double(int(v))) {
        out.error = "\"fixed_threshold\" must be an integer in [2, 255]";
        return out;
      }
      out.cfg.threshold = onpair::encoding::FixedThreshold{uint8_t(v)};
    } else if (key == "seed") {
      out.cfg.seed = uint64_t(v);
    } else {
      out.error = "unknown config key \"" + std::string(key) + "\"";
      return out;
    }
    p = skip_ws(p);
    if (*p == ',') {
      p = skip_ws(p + 1);
      continue;
    }
    if (*p == '}') return out;
    out.error = "expected ',' or '}'";
    return out;
  }
}

// --------------------------------------------------------------- candidate

struct Handle {
  onpair::OnPairColumn col;
  uint64_t num_rows = 0;
  uint64_t payload_bytes = 0;  // canonical chunk payload, offsets[num_rows]
};

void* onpair_build(const lb_chunk_view* view, const char* config_json,
                   char* err_buf, uint64_t err_cap) {
  auto fail = [&](const std::string& msg) -> void* {
    if (err_cap > 0) std::snprintf(err_buf, err_cap, "%s", msg.c_str());
    return nullptr;
  };
  const ParsedConfig parsed = parse_config(config_json);
  if (!parsed.error.empty()) return fail(parsed.error);

  const uint64_t n = view->num_rows;
  const uint64_t payload = view->offsets[n];
  if (payload > UINT32_MAX) {
    return fail("chunk payload " + std::to_string(payload) +
                " bytes exceeds OnPair's uint32_t offsets; set a smaller "
                "measure.chunk_rows");
  }
  try {
    // Narrow the contract's uint64_t offsets to OnPair's uint32_t.
    std::vector<uint32_t> offsets32(n + 1);
    for (uint64_t i = 0; i <= n; i++) offsets32[i] = uint32_t(view->offsets[i]);
    auto h = std::make_unique<Handle>();
    h->col = onpair::OnPairColumn::compress(
        reinterpret_cast<const char*>(view->bytes), offsets32.data(),
        size_t(n), parsed.cfg);
    h->num_rows = n;
    h->payload_bytes = payload;
    return h.release();
  } catch (const std::exception& e) {
    return fail(std::string("compress failed: ") + e.what());
  } catch (...) {
    return fail("compress failed: unknown exception");
  }
}

uint32_t onpair_footprint(void* self, lb_footprint_component* out,
                          uint32_t capacity) {
  auto* h = static_cast<Handle*>(self);
  const onpair::OnPairColumnView v = h->col.view();
  const onpair::StoreView sv = v.store();
  const onpair::DictionaryView dv = v.dictionary();
  // Mirrors Store::bytes_used / Dictionary::bytes_used exactly, split into
  // named components: the token stream is the payload-analog, the two
  // uint32 arrays are the offsets-analogs, and the dictionary is the
  // shared learned state.
  const uint64_t packed = (uint64_t(sv.num_tokens()) * sv.bits() + 7) / 8;
  const uint64_t boundaries = uint64_t(sv.bytes_used()) - packed;
  const uint64_t ntok = dv.num_tokens();
  const uint64_t dict_bytes = ntok ? dv.raw_offsets()[ntok] : 0;
  const uint64_t dict_offsets = uint64_t(dv.bytes_used()) - dict_bytes;
  const lb_footprint_component components[] = {
      {"token_stream", packed},
      {"boundaries", boundaries},
      {"dict_bytes", dict_bytes},
      {"dict_offsets", dict_offsets},
  };
  const uint32_t count = 4;
  for (uint32_t i = 0; i < count && i < capacity; i++) out[i] = components[i];
  return count;
}

int onpair_decode(void* self, uint8_t* bytes_out, uint64_t bytes_cap,
                  uint64_t* offsets_out) {
  auto* h = static_cast<Handle*>(self);
  // decompress_all over-copies up to DECOMPRESS_BUFFER_PADDING bytes past
  // the final row; the contract's pad covers it.
  static_assert(onpair::DECOMPRESS_BUFFER_PADDING <= LB_DECODE_PAD);
  if (bytes_cap < h->payload_bytes + onpair::DECOMPRESS_BUFFER_PADDING) return 1;
  const onpair::OnPairColumnView v = h->col.view();
  auto* o32 = reinterpret_cast<uint32_t*>(offsets_out);
  const size_t decoded =
      v.decompress_all(reinterpret_cast<char*>(bytes_out), o32);
  if (decoded != h->payload_bytes) return 2;
  // Widen the num_rows + 1 uint32_t offsets to the contract's uint64_t in
  // place, back to front: the u64 write to slot i lands on u32 slots 2i
  // and 2i+1, both >= i, so every u32 is read before it is clobbered.
  // No scratch, no allocation in the timed path.
  for (uint64_t i = h->num_rows + 1; i-- > 0;) {
    uint32_t val;
    std::memcpy(&val, reinterpret_cast<const uint8_t*>(offsets_out) + i * sizeof(uint32_t),
                sizeof(val));
    offsets_out[i] = val;
  }
  return 0;
}

// The "compressed" strategy: compile the needles into a token automaton
// against this chunk's dictionary, then drive it over the packed stream.
// Compilation happens inside every call — it is per-query work, exactly
// like scanner prepare() (SEMANTICS.md rule 1: no memoization). In
// instrumented mode (stats non-null) the compilation is self-timed into
// setup_ns (ABI v3) — the automata precompute eagerly in their
// constructors, so the constructor return is exactly the setup/scan joint.
// In timing mode the clock never runs, keeping timed samples free of
// clock reads (rule 5: identical call shape, no bookkeeping).
int onpair_run(void* self, uint32_t strategy_index, const lb_query* query,
               uint64_t* out_bitmap_words, lb_run_stats* stats_or_null) {
  auto* h = static_cast<Handle*>(self);
  if (strategy_index != 0) return 10;
  const onpair::OnPairColumnView v = h->col.view();
  auto set_bit = [out_bitmap_words](size_t row) {
    out_bitmap_words[row >> 6] |= uint64_t(1) << (row & 63);
  };
  auto needle = [query](uint32_t i) -> std::string_view {
    const lb_bytes& n = query->needles[i];
    return n.len ? std::string_view(reinterpret_cast<const char*>(n.ptr),
                                    size_t(n.len))
                 : std::string_view();
  };
  using Clock = std::chrono::steady_clock;
  const auto setup_start = stats_or_null ? Clock::now() : Clock::time_point{};
  auto setup_done = [&] {
    if (stats_or_null) {
      stats_or_null->setup_ns = uint64_t(
          std::chrono::duration_cast<std::chrono::nanoseconds>(Clock::now() -
                                                               setup_start)
              .count());
    }
  };
  try {
    switch (query->op) {
      case LB_PREFIX: {
        onpair::search::PrefixAutomaton pa(needle(0), v.dictionary());
        setup_done();
        v.scan(pa, set_bit);
        return 0;
      }
      case LB_CONTAINS: {
        const std::string_view n = needle(0);
        // KmpAutomaton stores its states as uint8_t.
        if (n.size() > 255) return 11;
        onpair::search::KmpAutomaton kmp(n, v.dictionary());
        setup_done();
        v.scan(kmp, set_bit);
        return 0;
      }
      case LB_CONTAINS_ANY: {
        std::vector<std::string_view> patterns(query->needle_count);
        for (uint32_t i = 0; i < query->needle_count; i++) patterns[i] = needle(i);
        onpair::search::AhoCorasickAutomaton ac(patterns, v.dictionary());
        setup_done();
        v.scan(ac, set_bit);
        return 0;
      }
      default:
        return 12;  // not in supported_ops; the harness never sends these
    }
  } catch (...) {
    return 13;
  }
}

void onpair_destroy(void* self) { delete static_cast<Handle*>(self); }

const lb_strategy kStrategies[] = {
    {"compressed",
     LB_OP_BIT(LB_PREFIX) | LB_OP_BIT(LB_CONTAINS) | LB_OP_BIT(LB_CONTAINS_ANY)},
};

const lb_candidate kVtable = {
    /*abi_version=*/LB_ABI_VERSION,
    /*name=*/"onpair",
    /*version=*/"0.1.0+f8ecc64",
    /*cpu_features=*/nullptr,
    /*strategies=*/kStrategies,
    /*strategy_count=*/1,
    /*build=*/onpair_build,
    /*footprint=*/onpair_footprint,
    /*run=*/onpair_run,
    /*view=*/nullptr,  // stored form is not the canonical layout
    /*decode=*/onpair_decode,
    /*destroy=*/onpair_destroy,
};

}  // namespace

extern "C" const lb_candidate* lb_candidate_onpair(void) { return &kVtable; }
