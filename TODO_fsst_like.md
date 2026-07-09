# TODO: `fsst_like` codegen backends (cpp / llvm / SIMD) — x86 + LLVM 16 host

**Audience:** a coding agent on an **x86-64 Linux (or x86 mac) host with LLVM 16
installed**. You are picking up the `fsst_like` candidate where the arm64 dev box
left off. Read this whole file, then `DESIGN.md` §17, before touching code.

## TL;DR

`candidates/fsst_like` wires the DaMoN'26 **FSST-LIKE-Matching** compressed-domain
LIKE matcher. The **interpreted** backend (`interp` strategy) is **done, tested,
and passes the correctness gate on all four columns**. Your job: add the three
**codegen** backends the paper's headline numbers come from —

| strategy   | backend | gate |
|------------|---------|------|
| `cpp`      | C++ codegen → `clang++ -shared` → `dlopen` (no SIMD) | a runtime C++ compiler exists |
| `cpp-simd` | as `cpp`, SSE `_mm_cmpestrm`/`_mm_cmpeq_epi8` | + `__x86_64__` |
| `llvm`     | LLVM ORC LLJIT of the automaton (no SIMD) | built with LLVM 16 (`HAVE_LLVM`) |
| `llvm-simd`| as `llvm`, SSE intrinsics | + `HAVE_LLVM` && `__x86_64__` |

They were deferred here because **this dev box is arm64 with no LLVM 16**: the
SIMD paths use x86-only intrinsics, and LLVM 16 wasn't installed — untestable
blind, same call the project already made for `sse4-strstr`/`vectorscan`
(DESIGN §16.4).

**Why it matters:** the interpreted automaton *loses* to decode-then-eval on
long rows (dbpedia 126 ms vs 85 ms) because its per-symbol overhead scales with
row length. Codegen is what makes FSST-LIKE competitive there; without it the
paper under-sells the technique. Short/medium rows already win with interp
(msmarco 13.4 vs 19.6 ms; url 22.6 vs 26.5; tpch 26.8 vs 31.7).

## Prereqs on your host

- **LLVM 16** exactly: `apt install llvm-16-dev` (Linux) or `brew install llvm@16`.
  Confirm `llvm-config-16 --version` (or `$(brew --prefix llvm@16)/bin/llvm-config`).
- A C++20 `clang++` on `PATH` (for the `cpp` backend's runtime compile).
- Rust toolchain + CMake ≥ 3.21 (already used by the repo).
- `./reproduce.sh datasets` if the datasets aren't present.

## What already exists (don't rebuild it)

- `candidates/fsst_like/cpp/fsst_like_candidate.cpp` — the vtable. `build()`
  trains an FSST table, compresses rows, writes the escaped-byte bitmap into
  `symbols[255]`, builds a FSST-LIKE `Encoder`. `run()` currently implements only
  strategy 0 = `interp` via `LikePatternAutomatonParser`. `decode()` uses
  `fsst_import`+`fsst_decompress`. `footprint()` reports `payload_fsst` +
  `symbol_table` + `offsets`.
- `candidates/fsst_like/cpp/CMakeLists.txt` — FetchContent-pins FSST-LIKE
  (`b1eb3ab9…`), calin2110/fsst (`1755328b…`), fmt (`12.1.0`); compiles the
  **interpreted subset** and **localizes all symbols except `lb_candidate_fsst_like`**
  (see "Symbol localization" below — this is load-bearing, keep it).
- `candidates/fsst_like/{Cargo.toml,build.rs,src/lib.rs}` — standard C++-candidate
  glue (mirrors `candidates/onpair`). Registered in `harness/src/registry.rs`,
  workspace `Cargo.toml`, `harness/Cargo.toml` (`cand-fsst-like`).
- Specs: `specs/compression/codecs.toml` (fsst_like on the compression axis,
  `decode`) and `specs/compression/fsst_like_query.toml` (query axis:
  `strategies = ["interp","compressed","decode"]`). **Add `cpp`,`cpp-simd`,`llvm`,
  `llvm-simd` to that allowlist once wired.**

## The FSST-LIKE codegen API (exact usage)

From the upstream `benchmark/benchmark_utils.hpp` (your reference for wiring):

```cpp
#include "codegen/cppcodegen.hpp"     // automata::codegen::cpp::{CppCompiler,CppParser}
#include "codegen/llvmcodegen.hpp"    // automata::codegen::llvmir::{LLVMCompiler,LLVMParser}
#include "like_pattern_automaton.hpp" // automata::parsing::{LikePatternAutomaton,LikePatternAutomatonParser}

// Build the automaton from the LIKE pattern + Encoder (per-query; this is "createTime").
std::unique_ptr<automata::parsing::LikePatternAutomaton> a =
    automata::parsing::LikePatternAutomaton::build(pattern_span, encoder);

// --- C++ codegen backend ---   (useSIMD = false for `cpp`, true for `cpp-simd`)
automata::codegen::cpp::CppCompiler cc{cppFile, soPath, /*enableSIMD=*/useSIMD};
std::unique_ptr<automata::codegen::Parser> p = cc.compile(a);      // writes cppFile, runs clang++, dlopens soPath
auto* cpp = dynamic_cast<automata::codegen::cpp::CppParser*>(p.get());
bool hit = cpp->parse(row_ptr, row_len);                            // per compressed row

// --- LLVM JIT backend ---      (useSIMD = false for `llvm`, true for `llvm-simd`)
automata::codegen::llvmir::LLVMCompiler lc{/*enableSIMD=*/useSIMD};
std::unique_ptr<automata::codegen::Parser> p = lc.compile(a);
auto* jit = dynamic_cast<automata::codegen::llvmir::LLVMParser*>(p.get());
bool hit = jit->parse(row_ptr, row_len);
```

- `CppCompiler::compile` runs (hardcoded in upstream `src/codegen/cppcodegen.cpp`):
  `clang++ -march=native -O3 -g -std=c++20 -shared -o <destination> -fPIC -I../ <cppFile>`
  then `dlopen("./<destination>")`. **The generated `.cpp` is self-contained**
  (only `<cstddef>/<cstdint>/<bit>/<string_view>` + `<nmmintrin.h>/<pmmintrin.h>`
  when SIMD) — it does NOT include FSST-LIKE headers, so no headers need shipping.
  `~CppCompiler` removes `cppFile` and `destination` (the dlopen handle stays
  valid after unlink). **Caveat you must fix:** the paths are used verbatim and
  `dlopen` prepends `"./"`, so `destination` must be **CWD-relative** and
  **unique per call** (upstream hardcodes `../automaton_bm.cpp` /
  `libgenerated_bm.so` — not concurrency-safe, pollutes CWD). Redirect both to a
  unique path under a scratch dir; simplest robust option is to `chdir` into a
  per-build scratch dir for the compile (the harness runs candidates
  single-threaded) or generate bare unique names (`lb_fl_<pid>_<counter>.{cpp,so}`)
  and clean them up. **Do not** pass an absolute `destination` (the `"./"` prepend
  breaks it).
- `LLVMCompiler` uses `llvm::orc::LLJIT`. Call `llvm::InitializeNativeTarget();
  llvm::InitializeNativeAsmPrinter();` **once** (guard with `std::once_flag`)
  before the first compile. `LLVMParser` holds the JIT and the function pointer.

The compiled function is `bool(*)(const uint8_t*, size_t)` — same signature the
interpreted `LikePatternAutomatonParser::parse` presents. `run()` loops rows and
sets the bitmap bit on a hit, identically to the current interp path.

## Per-query cost accounting

`create + compile/JIT` is the paper's preprocess/compile cost. Self-time it into
`stats->setup_ns` (ABI v3) exactly as the current `interp` path times the
`LikePatternAutomatonParser` constructor. Headline latency includes it; the
harness reports `setup_ns` alongside so match-only = headline − setup. For
codegen this dominates a single query and amortizes over rows — that is the point.

## Making the strategy list dynamic + gated (replaces the static array)

Today `kStrategies` is a static one-entry array (`interp`). Replace it with a
list assembled **at first vtable access** (function-local `static`) containing
only the viable strategies for this build/host, e.g.:

```cpp
const uint32_t OPS = LB_OP_BIT(LB_PREFIX)|LB_OP_BIT(LB_SUFFIX)
                   | LB_OP_BIT(LB_CONTAINS)|LB_OP_BIT(LB_MULTI_CONTAINS);
static std::vector<lb_strategy> strategies;   // stable storage for the vtable
strategies.push_back({"interp", OPS});                       // always
if (runtime_cxx_compiler_found()) strategies.push_back({"cpp", OPS});
#if defined(__x86_64__)
if (runtime_cxx_compiler_found()) strategies.push_back({"cpp-simd", OPS});
#endif
#ifdef HAVE_LLVM
strategies.push_back({"llvm", OPS});
#  if defined(__x86_64__)
strategies.push_back({"llvm-simd", OPS});
#  endif
#endif
// vtable.strategies = strategies.data(); vtable.strategy_count = strategies.size();
```

`run()` then dispatches on `strategy_index` → the matching backend, mapping the
index back to the strategy name/kind you registered (keep an aligned array of
backend kinds). **Unavailable backends are simply not declared** — the harness
records those cells Unavailable/skips them; never error, never scalar-substitute
(DESIGN §9/§16.2 policy). The `-simd` strategies must be **absent** (not just
erroring) on arm64.

`runtime_cxx_compiler_found()`: probe once (e.g. `system("clang++ --version
>/dev/null 2>&1") == 0`), cache the result. If absent, drop `cpp`/`cpp-simd`.

## CMake / build changes

In `candidates/fsst_like/cpp/CMakeLists.txt`:

1. Add the codegen sources to the `fl_objs` OBJECT library:
   `${fsst_like_src_SOURCE_DIR}/src/codegen/codegen.cpp`,
   `.../src/codegen/cppcodegen.cpp`, and (LLVM only)
   `.../src/codegen/llvmcodegen.cpp`. Add `.../src/utils.cpp` if the linker asks
   for it (cppcodegen pulls `utils.hpp`; most of it is header-only templates like
   `loadUnaligned`, but check).
2. LLVM detection + `HAVE_LLVM`:
   ```cmake
   find_package(LLVM 16 CONFIG)   # set LLVM_DIR / CMAKE_PREFIX_PATH to llvm@16 if needed
   if(LLVM_FOUND)
     target_compile_definitions(fl_objs PRIVATE HAVE_LLVM ${LLVM_DEFINITIONS})
     target_include_directories(fl_objs PRIVATE ${LLVM_INCLUDE_DIRS})
     llvm_map_components_to_libnames(LLVM_LIBS core orcjit native nativecodegen)
     # link LLVM_LIBS into the final lb_fsst_like target (see localization note)
   endif()
   ```
   Surface `HAVE_LLVM` to `build.rs`/the `.cpp` so the strategy list matches what
   was compiled. `dlopen`/`dlclose` need `-ldl` on Linux (`target_link_libraries
   ... ${CMAKE_DL_LIBS}`).
3. Keep `-fvisibility=hidden`, `-Wno-deprecated-declarations`, `-Wno-return-type`,
   `SHELL:-include unistd.h`, and the `fsst_avx512.cpp @ -O1` override.

## Symbol localization (load-bearing — keep it, extend it)

`fsst_like` and the `fsst` candidate each statically link a **different, full
FSST copy** (calin2110 vs cwida upstream), and both forks export the *identical*
`fsst_*` C API **and** `libfsst::…` C++ symbols → a duplicate-symbol link error
if left global. The CMake already fixes this by combining all objects with
`ld -r` and exporting **only** `lb_candidate_fsst_like` (macOS:
`ld -r -exported_symbol _lb_candidate_fsst_like`; Linux: `ld -r` +
`objcopy --keep-global-symbol=lb_candidate_fsst_like`). Verify with
`nm -gU <archive>` (or `nm -g --defined-only` on Linux) → only that one symbol.

When you add **LLVM**, its static libs bring thousands of symbols; localizing
all-but-entry-point still applies and keeps them from leaking/clashing. Confirm
the `nm` check after adding LLVM. (If LLVM's static archives make `ld -r` unhappy,
an alternative is to link LLVM normally but wrap the candidate's own objects; keep
the single-global-symbol invariant either way.)

## Gotchas already discovered (save yourself the debugging)

- **`libfsst.hpp` has NO include guard.** Include it exactly once — via
  `encoder.hpp`. Never `#include <fsst/libfsst.hpp>` directly. `fsst.h` *is*
  guarded (`FSST_INCLUDED_H`), include it for the C API.
- **`encoder.cpp`/`utils.cpp` use `read`/`write`/`close` without `<unistd.h>`**
  (compiles on their Linux GCC via transitive includes). The CMake force-includes
  it (`SHELL:-include unistd.h`); keep that.
- **`basic_string<uint8_t>`** in `encoder.hpp` is deprecated on libc++ (warning,
  not error) → `-Wno-deprecated-declarations`.
- **`fsst_avx512.cpp` must build at `-O1`** (upstream workaround; higher -O spills
  AVX-512 registers). Keep the per-source `COMPILE_FLAGS "-O1"`.
- **Pattern translation & escaping (already implemented in `to_like_pattern`):**
  escape `%`→`\%`, `_`→`\_`, `\`→`\\` in each needle, then wrap:
  PREFIX `n%`, SUFFIX `%n`, CONTAINS `%n%`, MULTI_CONTAINS `%a%b%…%`. This keeps
  the implemented `StringPattern` path (unescaped `_` → unimplemented
  `UnderscorePattern`, which throws). **Known limitation:** a needle ending in a
  literal backslash yields `…\\%` which the parser mis-reads (matches wrong);
  rare in real columns, verified absent in the current suites; the correctness
  gate is the backstop. CONTAINS_ANY is unsupported (not one LIKE pattern).
- **Escaped-byte bitmap:** `build()` writes a 256-entry bitmap of escaped bytes
  into `symbols[255]` (`isEscapable()` reads it) — mirrors the repo's
  `fa-drawing/server.cpp compressFile()`. Keep this; the matcher needs it.
- **Export before clobber:** `decode()` uses the symbol table serialized
  (`fsst_export`) **before** the `symbols[255]` clobber, so decode is clean/standard.
- **The build path itself** (`fsst_create` → grab `((libfsst::Encoder*)enc)
  ->symbolTable` → `fsst_compress` → clobber → `Encoder`) is validated. Don't
  change it; just add codegen strategies to `run()`.

## Pinned upstream commits

- FSST-LIKE-Matching: `b1eb3ab9c63ea0199a381b92371a3154190b4406`
- calin2110/fsst: `1755328b61f4e48ab7f53b315bbb20e8130059f2`
- fmt: tag `12.1.0`

## How to validate (must pass before reporting any number)

1. `cargo build --release` — both FSST candidates compile; `fsst_like` now
   registers `interp` + `cpp`(+`cpp-simd` on x86) + `llvm`(+`llvm-simd`) with
   LLVM 16 present. Verify the strategy list via a quick harness probe or by
   inspecting the run output's per-strategy cells.
2. Add the new strategy names to `specs/compression/fsst_like_query.toml`'s
   `strategies` allowlist. Run:
   `./target/release/bench run specs/compression/fsst_like_query.toml --out results/fsst_like_query`
   **Exit code must be 0** — the correctness gate (count + bitmap hash vs the
   uncompressed oracle) must pass for **every** codegen strategy on **every**
   suite query. Exit 3 = a gate failure; find the offending (dataset, strategy,
   query) in `results/fsst_like_query/results.jsonl`.
3. Compare latencies: codegen should beat `interp`, and should be competitive
   with (ideally beat) `decode` **on dbpedia (long rows)** — that's the whole
   point. `setup_ns` should be populated (large for `cpp` — per-query clang++;
   smaller for `llvm`). Fold into the report via
   `scratchpad/report_compression.py` or the query-axis analysis.
4. `cargo test` (harness `tests/pipeline.rs`) stays green.
5. Update `DESIGN.md` §17.6 status (mark codegen done, record the long-row result)
   and note in the roster that the x86 run happened.

## Useful references in-tree

- `candidates/onpair/` — the other compressed-domain C++ candidate (vtable,
  build.rs, CMake FetchContent pattern, `setup_ns` self-timing in `run()`).
- `contract/lb_candidate.h` + `contract/SEMANTICS.md` — the ABI (esp. `run`,
  `lb_run_stats.setup_ns`, strategy rules, no-memoization rule).
- `harness/src/registry.rs` — how strategies/candidates are validated and gated
  (`cpu_features` hard-gate; the strategy list gating happens candidate-side).
- Upstream `benchmark/measure_singlethreaded.cpp` — end-to-end usage of every
  backend (InMemory/CppCompiled/LLVMCompiled ± SIMD).
