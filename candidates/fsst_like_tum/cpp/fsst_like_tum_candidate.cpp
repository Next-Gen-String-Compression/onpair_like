// fsst_like_tum — FSST-LIKE-Matching (calin2110/FSST-LIKE-Matching, DaMoN'26;
// pinned in CMakeLists.txt): LIKE/substring predicates evaluated directly on
// FSST-compressed bytes via a per-pattern finite automaton (DESIGN.md §17).
//
// Match-in-place backends (all compressed-domain, all per-query setup):
//   - "interp": build the interpreted LikePatternAutomatonParser for the query,
//     drive parse() over each row's compressed bytes.
//   - "cpp" / "cpp-simd" (registered only when a runtime clang++ exists; -simd
//     additionally requires x86-64): the DaMoN'26 C++ codegen backend —
//     generate a specialized C++ kernel for the query automaton, clang++
//     -shared it, dlopen it, drive the compiled parse() per row.
//   - "llvm" / "llvm-simd" (registered only when built with LLVM, HAVE_LLVM;
//     -simd additionally x86-64): ORC LLJIT the automaton in-process.
// For every backend the per-query automaton build + codegen/compile/JIT cost is
// self-timed into setup_ns (DESIGN §17.4): headline latency includes it,
// match-only = headline − setup. This match-in-place path is the candidate's
// whole point, so it deliberately exposes NO decode(): it is a matcher, not a
// codec, and the decode-then-scan baseline is the `fsst` candidate's job.
//
// build() starts with the SHARED fsst_common front-end
// (candidates/fsst_common/fsst_build.hpp), identical to the `fsst` candidate.
// This file's cand_build() then does the one
// extra, fork-specific step the matcher needs: write the escaped-byte bitmap
// into symbols[255] (which the matcher's isEscapable() reads) and build the
// FSST-LIKE Encoder over the trained table. The FSST fork here (calin2110) is
// DISTINCT from the `fsst` candidate's cwida upstream; both statically link a
// full FSST copy, so this candidate's FSST symbols are localized at link (see
// CMakeLists.txt) to avoid a duplicate-symbol clash.
//
// Op -> LIKE pattern, escaping % _ \ inside needles so only the implemented
// StringPattern (start/middle/end/full) path is used, never the unimplemented
// UnderscorePattern (unescaped _). Known upstream limitation: a needle ending in
// a literal backslash that is followed by '%' in the pattern ("...\\%": every
// PREFIX / CONTAINS / MULTI_CONTAINS needle) is mis-read by the parser's
// end-detection as an escaped '%' and matched wrong (verified 2026-09-01 with
// an oracle driver; "%...\\" for SUFFIX is fine). run() refuses such queries
// with kErrTrailingBackslash instead of reporting a wrong bitmap. CONTAINS_ANY
// is unsupported (an OR of literals is not one LIKE pattern).
//
// Compressed-stream layout (DESIGN.md §17.7). The upstream kernels read ONE
// byte outside the row they are handed: the backward (suffix) scan reads the
// byte before the row when it parks in a level-0 pseudo-end state at the row
// start, and the LLVM kernels load data[strIdx] before the state switch, so
// they read the byte after the row when they park in an accept/error state at
// strIdx == len. Both were measured with a guard-page driver (at most 1 byte
// each side; the C++/interp forward paths never over-read). The byte AFTER the
// row never influences the answer; the byte BEFORE does: 0xFF there makes the
// backward scan treat the row's first code as an escaped literal (false
// negative). Upstream's own benchmark never sees this because its rows live
// in a page-padded mmap. Here the rows are therefore laid out by the shared
// fsst_common::GuardedStream (guard blocks, plus a 0x00 separator after every
// row whenever any compressed row ends in 0xFF), whose padding is reported as
// the `stream_padding` footprint component (0 separators on all four benchmark
// columns, which contain no 0xFF byte). dict_fsst_like_tum uses the same helper
// for the same reason. Since 2026-09-02 the pinned kernels (fork branch behind
// upstream PR #1) no longer read before the row and decide escapes by the
// parity of the 255 run, which also fixed the in-row false negatives (raw
// "\xFFe" LIKE '%e', "\xFF\xFF\xFFthe" LIKE '%he%'); the layout is kept as belt
// and braces and for the LLVM after-row load, which is unchanged.

#include "lb_candidate.h"

// libfsst.hpp has NO include guard; encoder.hpp includes it exactly once. Never
// include <fsst/libfsst.hpp> directly here. fsst.h IS guarded (FSST_INCLUDED_H).
#include "codegen/cppcodegen.hpp"
#include "encoder.hpp"
#include "like_pattern_automaton.hpp"
#include <fsst/fsst.h>

#include "fsst_build.hpp"  // shared front-end; include AFTER <fsst/fsst.h>

#ifdef HAVE_LLVM
#include "codegen/llvmcodegen.hpp"
#include "llvm/Support/TargetSelect.h"
#endif

#include <atomic>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <memory>
#include <mutex>
#include <span>
#include <string>
#include <unistd.h>
#include <vector>

namespace {

// run() return codes (SEMANTICS.md error convention: nonzero = the cell
// errored and reports no numbers).
constexpr int kErrBadStrategy = 10;       // strategy_index out of range
constexpr int kErrUnsupportedOp = 11;     // op not expressible as one LIKE pattern
constexpr int kErrMatcherFailure = 12;    // upstream automaton/codegen threw
constexpr int kErrTrailingBackslash = 13; // needle ends in '\' before a '%'

// The shared FSST build result, the FSST-LIKE Encoder (over the clobbered
// table) that drives matching, and the guarded row stream the kernels actually
// read (see file header).
struct Handle {
  fsst_common::FsstBuilt b;
  std::unique_ptr<Encoder> encoder;
  fsst_common::GuardedStream stream;
};

// Drive `parse(ptr, len)` over every row, setting the row's bit on a hit.
template <class Parse>
void match_rows(const Handle& h, Parse&& parse, uint64_t* out_bitmap_words) {
  for (uint64_t i = 0; i < h.b.num_rows; i++) {
    if (parse(h.stream.row(i), h.stream.row_len(i))) {
      out_bitmap_words[i >> 6] |= uint64_t(1) << (i & 63);
    }
  }
}

// Append `nd` to `out` with LIKE metacharacters escaped, so it is matched as a
// literal substring (never a wildcard).
void escape_append(std::vector<uint8_t>& out, const lb_bytes& nd) {
  for (uint64_t i = 0; i < nd.len; i++) {
    const uint8_t c = nd.ptr[i];
    if (c == '%' || c == '_' || c == '\\') out.push_back('\\');
    out.push_back(c);
  }
}

// True when needle `i` ends in a literal backslash; such a needle followed by
// '%' is mis-parsed upstream (see file header).
bool ends_in_backslash(const lb_query* q, uint32_t i) {
  const lb_bytes& nd = q->needles[i];
  return nd.len > 0 && nd.ptr[nd.len - 1] == '\\';
}

// (op, needles) -> LIKE pattern bytes. Returns 0, or the run() error code for
// a query this matcher cannot express (CONTAINS_ANY is never sent, being
// absent from supported_ops) or cannot evaluate correctly.
int to_like_pattern(const lb_query* q, std::vector<uint8_t>& pat) {
  auto nd = [q](uint32_t i) { return q->needles[i]; };
  switch (q->op) {
    case LB_PREFIX:
      if (ends_in_backslash(q, 0)) return kErrTrailingBackslash;
      escape_append(pat, nd(0));
      pat.push_back('%');
      return 0;
    case LB_SUFFIX:  // the needle is last in the pattern: no '%' follows it
      pat.push_back('%');
      escape_append(pat, nd(0));
      return 0;
    case LB_CONTAINS:
      if (ends_in_backslash(q, 0)) return kErrTrailingBackslash;
      pat.push_back('%');
      escape_append(pat, nd(0));
      pat.push_back('%');
      return 0;
    case LB_MULTI_CONTAINS:
      pat.push_back('%');
      for (uint32_t i = 0; i < q->needle_count; i++) {
        if (ends_in_backslash(q, i)) return kErrTrailingBackslash;
        escape_append(pat, nd(i));
        pat.push_back('%');
      }
      return 0;
    default:
      return kErrUnsupportedOp;
  }
}

void* cand_build(const lb_chunk_view* view, const char* /*config_json*/,
                 char* err_buf, uint64_t err_cap) {
  auto fail = [&](const char* msg) -> void* {
    if (err_cap > 0) std::snprintf(err_buf, err_cap, "%s", msg);
    return nullptr;
  };

  auto h = std::make_unique<Handle>();
  // Shared front-end: train + compress + concatenate + export. Leaves h->b.enc
  // alive for the automaton construction below.
  if (!fsst_common::Build(view, h->b, err_buf, err_cap)) return nullptr;

  try {
    // The libfsst::SymbolTable this encoder trained. Shared: keeping a copy of
    // the shared_ptr keeps it alive after fsst_destroy(enc).
    std::shared_ptr<libfsst::SymbolTable> sym =
        reinterpret_cast<libfsst::Encoder*>(h->b.enc)->symbolTable;

    // Escaped-byte bitmap into symbols[255], exactly as the repo's compressFile:
    // isEscapable(b) reads reinterpret_cast<bool*>(&symbols[255])[b].
    bool bitmap[256] = {false};
    const bool ends_in_escape = fsst_common::ScanEscapes(h->b, bitmap);
    std::memcpy(&sym->symbols[255], bitmap, 256 * sizeof(bool));
    fsst_common::LayoutGuardedStream(h->b, h->stream, ends_in_escape);

    // FSST-LIKE Encoder over the (clobbered) table, for automaton construction.
    SymbolTable st(sym);
    h->encoder = std::make_unique<Encoder>(st);

    fsst_destroy(h->b.enc);
    h->b.enc = nullptr;
    // Match-in-place needs compressed-row boundaries only. The canonical
    // decoded index was a shared-builder intermediate.
    fsst_common::ReleaseDecodedOffsets(h->b);
    return h.release();
  } catch (const std::exception& e) {
    if (h->b.enc) fsst_destroy(h->b.enc);
    return fail((std::string("build failed: ") + e.what()).c_str());
  } catch (...) {
    if (h->b.enc) fsst_destroy(h->b.enc);
    return fail("build failed: unknown exception");
  }
}

// The three shared FSST-family components, plus the retained FSST-LIKE encoder
// table (an expanded compression-side table kept for per-query automaton
// construction; counted next to the serialized dictionary just as fsst counts
// its imported decode table) and the guard/separator bytes of the stream layout.
uint32_t cand_footprint(void* self, lb_footprint_component* out,
                        uint32_t capacity) {
  auto* h = static_cast<Handle*>(self);
  uint32_t n = fsst_common::Footprint(h->b, out, capacity);
  const lb_footprint_component extra[] = {
      {"encoder_table", sizeof(libfsst::Encoder) + sizeof(libfsst::SymbolTable)},
      {"stream_padding", h->stream.padding_bytes()},
  };
  for (const lb_footprint_component& c : extra) {
    if (n < capacity) out[n] = c;
    n++;
  }
  return n;
}

void set_all_bits(uint64_t num_rows, uint64_t* out_bitmap_words) {
  for (uint64_t row = 0; row < num_rows; row++) {
    out_bitmap_words[row >> 6] |= uint64_t(1) << (row & 63);
  }
}

// "interp" strategy: build the automaton for this query (per-query setup,
// self-timed into setup_ns like a scanner prepare()), then drive it over the
// compressed rows. No memoization across calls (SEMANTICS.md rule 1).
int run_interp(void* self, const lb_query* query, uint64_t* out_bitmap_words,
               lb_run_stats* stats_or_null) {
  auto* h = static_cast<Handle*>(self);
  std::vector<uint8_t> pat;
  if (const int rc = to_like_pattern(query, pat); rc != 0) return rc;

  using Clock = std::chrono::steady_clock;
  const auto setup_start = stats_or_null ? Clock::now() : Clock::time_point{};

  try {
    automata::parsing::LikePatternAutomatonParser parser(
        std::span<const uint8_t>(pat.data(), pat.size()), *h->encoder);
    if (stats_or_null) {
      stats_or_null->setup_ns = uint64_t(
          std::chrono::duration_cast<std::chrono::nanoseconds>(Clock::now() -
                                                               setup_start)
              .count());
    }
    match_rows(*h, [&parser](const uint8_t* p, size_t len) {
      return parser.parse(std::span<const uint8_t>(p, len));
    }, out_bitmap_words);
    return 0;
  } catch (...) {
    return kErrMatcherFailure;
  }
}

// Unique, CWD-relative paths for one generated kernel. CppCompiler passes
// `destination` to dlopen with a "./" prefix, so it must stay CWD-relative,
// and dlopen caches handles BY PATHNAME, so every call needs a fresh name
// (pid makes concurrent harness processes sharing a CWD safe; the counter
// makes calls within this process unique). Prefer $TMPDIR (or /tmp) expressed
// relative to the CWD to keep the harness CWD clean; fall back to bare names
// in the CWD, which ~CppCompiler unlinks anyway.
struct TempNames {
  std::string cpp_file;
  std::string so_file;
};
TempNames make_temp_names() {
  static std::atomic<uint64_t> counter{0};
  const std::string base =
      "lb_fl_" + std::to_string(getpid()) + "_" +
      std::to_string(counter.fetch_add(1, std::memory_order_relaxed));
  namespace fs = std::filesystem;
  std::error_code ec;
  const char* tmpdir = std::getenv("TMPDIR");
  const fs::path dir = (tmpdir && *tmpdir) ? fs::path(tmpdir) : fs::path("/tmp");
  const fs::path rel = fs::relative(dir / base, fs::current_path(), ec);
  const std::string stem = (!ec && !rel.empty()) ? rel.string() : base;
  return {stem + ".cpp", stem + ".so"};
}

// All-empty needles yield a bare '%...%' pattern with no automaton content; the
// generated NO_DIRECTION kernel returns false, but LIKE '%%' matches every row.
// The codegen paths short-circuit this case before compiling anything.
bool all_needles_empty(const lb_query* query) {
  for (uint32_t i = 0; i < query->needle_count; i++) {
    if (query->needles[i].len != 0) return false;
  }
  return true;
}

// Strategies "cpp"/"cpp-simd": the DaMoN'26 C++ codegen backend. Build the
// query automaton, emit a specialized C++ kernel, `clang++ -shared` it, dlopen
// it, and drive the compiled parse() over each row. automaton + codegen +
// clang++ + dlopen is the paper's per-query compile cost — self-timed into
// setup_ns. A failed compile leaves no .so, so compile() throws (dlopen null)
// -> 12; the generated .cpp/.so are unlinked by ~CppCompiler on all paths.
int run_codegen(void* self, const lb_query* query, bool simd,
                uint64_t* out_bitmap_words, lb_run_stats* stats_or_null) {
  auto* h = static_cast<Handle*>(self);
  std::vector<uint8_t> pat;
  if (const int rc = to_like_pattern(query, pat); rc != 0) return rc;
  if (all_needles_empty(query)) {
    set_all_bits(h->b.num_rows, out_bitmap_words);
    return 0;
  }

  using Clock = std::chrono::steady_clock;
  const auto setup_start = stats_or_null ? Clock::now() : Clock::time_point{};
  try {
    std::unique_ptr<automata::parsing::LikePatternAutomaton> automaton =
        automata::parsing::LikePatternAutomaton::build(
            std::span<const uint8_t>(pat.data(), pat.size()), *h->encoder);
    const TempNames names = make_temp_names();
    // Declared before `parser` so destruction dlcloses the kernel first, then
    // unlinks the files.
    automata::codegen::cpp::CppCompiler compiler(names.cpp_file, names.so_file,
                                                 /*enableSIMD=*/simd);
    std::unique_ptr<automata::codegen::Parser> parser =
        compiler.compile(automaton);
    if (stats_or_null) {
      stats_or_null->setup_ns = uint64_t(
          std::chrono::duration_cast<std::chrono::nanoseconds>(Clock::now() -
                                                               setup_start)
              .count());
    }
    match_rows(*h, [&parser](const uint8_t* p, size_t len) {
      return parser->parse(p, len);
    }, out_bitmap_words);
    return 0;
  } catch (...) {
    return kErrMatcherFailure;
  }
}

#ifdef HAVE_LLVM
// Strategies "llvm"/"llvm-simd": ORC LLJIT the query automaton in-process —
// no external compiler, no filesystem artifacts. automaton + IR gen + JIT is
// the per-query compile cost, self-timed into setup_ns like the cpp backend.
int run_llvm(void* self, const lb_query* query, bool simd,
             uint64_t* out_bitmap_words, lb_run_stats* stats_or_null) {
  auto* h = static_cast<Handle*>(self);
  std::vector<uint8_t> pat;
  if (const int rc = to_like_pattern(query, pat); rc != 0) return rc;
  if (all_needles_empty(query)) {
    set_all_bits(h->b.num_rows, out_bitmap_words);
    return 0;
  }

  static std::once_flag llvm_init;
  std::call_once(llvm_init, [] {
    llvm::InitializeNativeTarget();
    llvm::InitializeNativeTargetAsmPrinter();
  });

  using Clock = std::chrono::steady_clock;
  const auto setup_start = stats_or_null ? Clock::now() : Clock::time_point{};
  try {
    std::unique_ptr<automata::parsing::LikePatternAutomaton> automaton =
        automata::parsing::LikePatternAutomaton::build(
            std::span<const uint8_t>(pat.data(), pat.size()), *h->encoder);
    automata::codegen::llvmir::LLVMCompiler compiler(/*enableSIMD=*/simd);
    std::unique_ptr<automata::codegen::Parser> parser =
        compiler.compile(automaton);
    if (stats_or_null) {
      stats_or_null->setup_ns = uint64_t(
          std::chrono::duration_cast<std::chrono::nanoseconds>(Clock::now() -
                                                               setup_start)
              .count());
    }
    match_rows(*h, [&parser](const uint8_t* p, size_t len) {
      return parser->parse(p, len);
    }, out_bitmap_words);
    return 0;
  } catch (...) {
    return kErrMatcherFailure;
  }
}
#endif  // HAVE_LLVM

// Registered strategy index -> backend. Filled alongside the strategy list in
// lb_candidate_fsst_like_tum(); indices past interp depend on build/host gating.
enum class Backend { kInterp, kCpp, kCppSimd, kLlvm, kLlvmSimd };
std::vector<Backend> g_backends;

int cand_run(void* self, uint32_t strategy_index, const lb_query* query,
             uint64_t* out_bitmap_words, lb_run_stats* stats_or_null) {
  if (strategy_index >= g_backends.size()) return kErrBadStrategy;
  switch (g_backends[strategy_index]) {
    case Backend::kInterp:
      return run_interp(self, query, out_bitmap_words, stats_or_null);
    case Backend::kCpp:
      return run_codegen(self, query, /*simd=*/false, out_bitmap_words,
                         stats_or_null);
    case Backend::kCppSimd:
      return run_codegen(self, query, /*simd=*/true, out_bitmap_words,
                         stats_or_null);
#ifdef HAVE_LLVM
    case Backend::kLlvm:
      return run_llvm(self, query, /*simd=*/false, out_bitmap_words,
                      stats_or_null);
    case Backend::kLlvmSimd:
      return run_llvm(self, query, /*simd=*/true, out_bitmap_words,
                      stats_or_null);
#endif
    default:
      return kErrBadStrategy;
  }
}

void cand_destroy(void* self) { delete static_cast<Handle*>(self); }

// One-shot cached probe for a runtime C++ compiler — a registration-time host
// capability check (like cpu_features), not per-query memoization. Without a
// clang++ on PATH the cpp/cpp-simd strategies are absent, not erroring
// (DESIGN §16.2: never error, never substitute).
bool runtime_cxx_compiler_found() {
  static const bool found =
      std::system("clang++ --version >/dev/null 2>&1") == 0;
  return found;
}

// Stable storage for the gated strategy list; the vtable points into these.
std::vector<lb_strategy> g_strategies;
lb_candidate g_vtable;

}  // namespace

// Default visibility so the CMake localize step (which hides every other FSST /
// FSST-LIKE symbol) can still export this single entry point.
// The strategy list is assembled once, on first access, with only the backends
// viable on this build/host: interp always; cpp when a runtime clang++ exists;
// llvm when built with LLVM (HAVE_LLVM); the -simd variants additionally
// require x86-64 (SSE4.2 intrinsics in the generated kernels).
extern "C" __attribute__((visibility("default"))) const lb_candidate*
lb_candidate_fsst_like_tum(void) {
  static const lb_candidate* vt = [] {
    constexpr uint32_t kLikeOps = LB_OP_BIT(LB_PREFIX) | LB_OP_BIT(LB_SUFFIX) |
                                  LB_OP_BIT(LB_CONTAINS) |
                                  LB_OP_BIT(LB_MULTI_CONTAINS);
    g_strategies = {{"interp", kLikeOps}};
    g_backends = {Backend::kInterp};
    if (runtime_cxx_compiler_found()) {
      g_strategies.push_back({"cpp", kLikeOps});
      g_backends.push_back(Backend::kCpp);
#if defined(__x86_64__)
      g_strategies.push_back({"cpp-simd", kLikeOps});
      g_backends.push_back(Backend::kCppSimd);
#endif
    }
#ifdef HAVE_LLVM
    g_strategies.push_back({"llvm", kLikeOps});
    g_backends.push_back(Backend::kLlvm);
#if defined(__x86_64__)
    g_strategies.push_back({"llvm-simd", kLikeOps});
    g_backends.push_back(Backend::kLlvmSimd);
#endif
#endif
    g_vtable = {
        /*abi_version=*/LB_ABI_VERSION,
        /*name=*/"fsst_like_tum",
        /*version=*/"0.4.0+b1eb3ab.guarded-stream",
        /*cpu_features=*/nullptr,
        /*strategies=*/g_strategies.data(),
        /*strategy_count=*/uint32_t(g_strategies.size()),
        /*build=*/cand_build,
        /*footprint=*/cand_footprint,
        /*run=*/cand_run,
        /*view=*/nullptr,   // stored form is not the canonical layout
        /*decode=*/nullptr, // interp-only matcher: no decode-then-scan path (that
                            // is the `fsst` candidate). Compressed-domain match
                            // only.
        /*destroy=*/cand_destroy,
        /*query_facts=*/nullptr,     // no prefilter cover to report
        /*export_artifact=*/nullptr, // no artifact format
    };
    return &g_vtable;
  }();
  return vt;
}
