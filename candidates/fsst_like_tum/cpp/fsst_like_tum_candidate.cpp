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
// build() and footprint() are the SHARED fsst_common front-end
// (candidates/fsst_common/fsst_build.hpp) — identical to the `fsst` candidate.
// This file's cand_build() just calls fsst_common::Build() and then does the one
// extra, fork-specific step the matcher needs: write the escaped-byte bitmap
// into symbols[255] (which the matcher's isEscapable() reads) and build the
// FSST-LIKE Encoder over the trained table. The FSST fork here (calin2110) is
// DISTINCT from the `fsst` candidate's cwida upstream; both statically link a
// full FSST copy, so this candidate's FSST symbols are localized at link (see
// CMakeLists.txt) to avoid a duplicate-symbol clash.
//
// Op -> LIKE pattern, escaping % _ \ inside needles so only the implemented
// StringPattern (start/middle/end/full) path is used, never the unimplemented
// UnderscorePattern (unescaped _). Known limitation: a needle ending in a
// literal backslash yields "...\\%", which the parser's end-detection mis-reads
// as an escaped % — such needles (rare in real columns) are matched wrong; the
// correctness gate is the backstop. CONTAINS_ANY is unsupported (an OR of
// literals is not one LIKE pattern).

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

// The shared FSST build result, plus the one fork-specific extra this candidate
// needs: the FSST-LIKE Encoder (over the clobbered table) that drives matching.
struct Handle {
  fsst_common::FsstBuilt b;
  std::unique_ptr<Encoder> encoder;
};

// Append `nd` to `out` with LIKE metacharacters escaped, so it is matched as a
// literal substring (never a wildcard).
void escape_append(std::vector<uint8_t>& out, const lb_bytes& nd) {
  for (uint64_t i = 0; i < nd.len; i++) {
    const uint8_t c = nd.ptr[i];
    if (c == '%' || c == '_' || c == '\\') out.push_back('\\');
    out.push_back(c);
  }
}

// (op, needles) -> LIKE pattern bytes. Returns false for CONTAINS_ANY (not one
// LIKE pattern) — never sent, since it is absent from supported_ops.
bool to_like_pattern(const lb_query* q, std::vector<uint8_t>& pat) {
  auto nd = [q](uint32_t i) { return q->needles[i]; };
  switch (q->op) {
    case LB_PREFIX:
      escape_append(pat, nd(0));
      pat.push_back('%');
      return true;
    case LB_SUFFIX:
      pat.push_back('%');
      escape_append(pat, nd(0));
      return true;
    case LB_CONTAINS:
      pat.push_back('%');
      escape_append(pat, nd(0));
      pat.push_back('%');
      return true;
    case LB_MULTI_CONTAINS:
      pat.push_back('%');
      for (uint32_t i = 0; i < q->needle_count; i++) {
        escape_append(pat, nd(i));
        pat.push_back('%');
      }
      return true;
    default:
      return false;
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
    // isEscapable(b) reads reinterpret_cast<bool*>(&symbols[255])[b]. Scan each
    // compressed row (escape 255 is always followed by its literal within the
    // same row, so the concatenated stream reads identically to per-row).
    bool bitmap[256] = {false};
    for (uint64_t i = 0; i < h->b.num_rows; i++) {
      const uint8_t* p = h->b.compressed.data() + h->b.coffsets[i];
      const size_t len = size_t(h->b.coffsets[i + 1] - h->b.coffsets[i]);
      size_t j = 0;
      while (j < len) {
        if (p[j] == 255) { ++j; bitmap[p[j]] = true; }
        ++j;
      }
    }
    std::memcpy(&sym->symbols[255], bitmap, 256 * sizeof(bool));

    // FSST-LIKE Encoder over the (clobbered) table, for automaton construction.
    SymbolTable st(sym);
    h->encoder = std::make_unique<Encoder>(st);

    fsst_destroy(h->b.enc);
    h->b.enc = nullptr;
    return h.release();
  } catch (const std::exception& e) {
    if (h->b.enc) fsst_destroy(h->b.enc);
    return fail((std::string("build failed: ") + e.what()).c_str());
  } catch (...) {
    if (h->b.enc) fsst_destroy(h->b.enc);
    return fail("build failed: unknown exception");
  }
}

uint32_t cand_footprint(void* self, lb_footprint_component* out,
                        uint32_t capacity) {
  return fsst_common::Footprint(static_cast<Handle*>(self)->b, out, capacity);
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
  if (!to_like_pattern(query, pat)) return 11;  // e.g. CONTAINS_ANY (unsupported)

  auto set_bit = [out_bitmap_words](uint64_t row) {
    out_bitmap_words[row >> 6] |= uint64_t(1) << (row & 63);
  };
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
    for (uint64_t i = 0; i < h->b.num_rows; i++) {
      const uint8_t* p = h->b.compressed.data() + h->b.coffsets[i];
      const size_t len = size_t(h->b.coffsets[i + 1] - h->b.coffsets[i]);
      if (parser.parse(std::span<const uint8_t>(p, len))) set_bit(i);
    }
    return 0;
  } catch (...) {
    return 12;
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
  if (!to_like_pattern(query, pat)) return 11;  // e.g. CONTAINS_ANY (unsupported)
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
    for (uint64_t i = 0; i < h->b.num_rows; i++) {
      const uint8_t* p = h->b.compressed.data() + h->b.coffsets[i];
      const size_t len = size_t(h->b.coffsets[i + 1] - h->b.coffsets[i]);
      if (parser->parse(p, len)) {
        out_bitmap_words[i >> 6] |= uint64_t(1) << (i & 63);
      }
    }
    return 0;
  } catch (...) {
    return 12;
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
  if (!to_like_pattern(query, pat)) return 11;  // e.g. CONTAINS_ANY (unsupported)
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
    for (uint64_t i = 0; i < h->b.num_rows; i++) {
      const uint8_t* p = h->b.compressed.data() + h->b.coffsets[i];
      const size_t len = size_t(h->b.coffsets[i + 1] - h->b.coffsets[i]);
      if (parser->parse(p, len)) {
        out_bitmap_words[i >> 6] |= uint64_t(1) << (i & 63);
      }
    }
    return 0;
  } catch (...) {
    return 12;
  }
}
#endif  // HAVE_LLVM

// Registered strategy index -> backend. Filled alongside the strategy list in
// lb_candidate_fsst_like_tum(); indices past interp depend on build/host gating.
enum class Backend { kInterp, kCpp, kCppSimd, kLlvm, kLlvmSimd };
std::vector<Backend> g_backends;

int cand_run(void* self, uint32_t strategy_index, const lb_query* query,
             uint64_t* out_bitmap_words, lb_run_stats* stats_or_null) {
  if (strategy_index >= g_backends.size()) return 10;
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
      return 10;
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
        /*version=*/"0.2.0+b1eb3ab",
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
    };
    return &g_vtable;
  }();
  return vt;
}
