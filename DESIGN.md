# Design: a two-axis benchmark for LIKE-family string predicates

Status: **reference spec — the design below is implemented (harness, candidates,
scanners; see [README.md](README.md)). Review rounds 1–3 (2026-07-07) incorporated;
all open questions resolved.**

This document is the architecture reference for the benchmark foundation. It
covers the language/integration decision, the dataset and
query formats, the load-and-prepare path, the candidate contract, correctness
gating, measurement methodology, prefilter inspection, the phase-1 vertical
slice, and open questions. Illustrative sketches (a C header, example JSON)
appear where a contract needs to be concrete to be evaluable; they are design
artifacts, not implementation.

---

## 1. Principles the design is built around

1. **A result is a (compression, latency) pair.** Everything downstream —
   result schema, reporting, even the candidate contract — treats footprint
   and latency as two first-class, separately-measured dimensions of one
   result. Neither is ever reported without the other being derivable.
2. **Identical input, identical measurement.** Every candidate consumes the
   same prepared bytes through the same view, and every candidate is timed by
   the same clock in the same loop. Differences in results must be
   attributable to the candidate, never to the harness path it happened to
   take. This principle drives the integration recommendation more than any
   other.
3. **Trust flows from one dumb oracle.** A deliberately naive reference scan
   is the single root of correctness. Ground truth is a cached, checksummed
   product of the oracle; candidates are gated against it; the gate itself is
   tested to fail.
4. **Instrumentation must never taint timing.** Prefilter attribution is
   collected in a separate instrumented pass, so the latency numbers are
   always from uninstrumented runs.
5. **The formats outlive phase 1.** The query format is designed for the
   future parameterized generator first and curated queries second — the
   generator is the harder client, and a format that serves it serves curated
   queries trivially.

---

## 2. Architecture at a glance

```
                     ingest (once, cached)                 bless (once, cached)
  raw source ──────────────────────────► canonical dataset ◄─────────────── suite
  (parquet/csv/tsv)                      artifact (+manifest)          (queries.jsonl
                                              │                        + suite.json,
                                              │ mmap                   truth bound to
                                              ▼                        dataset checksum)
                                    ┌───────────────────┐
                                    │  harness (Rust)   │
                                    │  ─ load & prepare │
                                    │  ─ oracle         │
                                    │  ─ timing loop    │
                                    │  ─ gate           │
                                    │  ─ results writer │
                                    └───────┬───────────┘
                              spawns one child process per (candidate, dataset)
                        ┌───────────────────┼───────────────────┐
                        ▼                   ▼                   ▼
                 ┌────────────┐      ┌────────────┐      ┌────────────┐
                 │ candidate  │      │ candidate  │      │ candidate  │
                 │ (Rust)     │      │ (C++)      │      │ (C++)      │
                 │ via C ABI  │      │ via C ABI  │      │ via C ABI  │
                 └────────────┘      └────────────┘      └────────────┘
                     in-process, statically linked, timed by the harness clock

  output: results.jsonl (one row per candidate × query) + run manifest (env,
  versions, spec, dataset/suite checksums)
```

The one-sentence summary: **a Rust harness owns formats, orchestration, the
oracle, and the clock; storage candidates *and scanners* (pluggable
uncompressed eval kernels, §7) in either language implement neutral C-ABI
contracts and run in-process; isolation between candidates comes from a
process-per-candidate run model, not from IPC.**

---

## 3. Language and integration: the decision

This is the load-bearing decision, so I'll lay out the space honestly. The
criteria, in the order I weighted them:

1. **Measurement fidelity** — can we time a sub-millisecond query without the
   integration mechanism appearing in the number?
2. **One timing code path** — is every candidate measured by literally the
   same loop and clock, or do we trust N reimplementations?
3. **Prefilter instrumentation** — how cheaply can structured per-query stats
   cross the candidate boundary?
4. **Ease of adding a candidate** — in each language, what does a new
   candidate author have to touch?
5. **Build complexity** — what does a fresh clone need to produce a runnable
   benchmark?
6. **Isolation** — can a segfaulting or allocator-corrupting candidate
   invalidate other candidates' numbers?

### Options considered

**(A) C++ harness, Rust candidates behind a C ABI.**
Symmetric in principle with (B). In practice the harness's actual workload is
not systems code: it is file formats, JSON, CLI, checksums, statistics,
process orchestration, and error propagation. The C++ ecosystem for that
(CMake + vcpkg/FetchContent, Arrow C++, nlohmann, CLI11) is markedly heavier
to build and to keep reproducible than the Rust equivalent (cargo + serde +
arrow-rs + clap, one lockfile). There is also an asymmetry in the candidate
libraries themselves: the C/C++ candidates you named (FSST, LZ4, dictionary
schemes) already expose C APIs and are trivially callable from anywhere,
whereas Vortex is a large Rust library whose scan path would need a bespoke
C-ABI wrapper to be called *from* C++ — the C++-harness direction wraps the
hard thing, the Rust-harness direction wraps things that are already wrapped.

**(B) Rust harness, all candidates behind a C ABI.** ← recommended
Same symmetric contract as (A) — the crucial point is that in both (A) and
(B), *neither candidate language is subordinate*, because candidates don't
target the harness language at all: they target a frozen C header. A C++
candidate implements `lb_candidate.h` with its own compiler and flags; a Rust
candidate implements the same header via `extern "C"` glue. The harness
language is an implementation detail behind the contract.

**(C) Subprocess candidates with an IPC protocol.**
Each candidate is a standalone binary; the harness feeds it queries over a
pipe/socket, data via a mmap'd file. Genuine advantages: total build
isolation (no cross-language linking ever), crash isolation, any-language
candidates for free. But it fails the two criteria I weighted highest.
Per-query IPC round-trips are the same order of magnitude as fast queries, so
timing must move *inside* the candidate process — which means every language
needs its own timing loop, warmup policy, and stats collection, and
"identical measurement" becomes "N implementations we hope are identical."
Self-reported timings from candidate authors are also exactly the trust
model a benchmark should avoid. Prefilter instrumentation becomes a protocol
instead of a struct. I rejected it, but deliberately captured its two real
benefits (build and crash isolation) in the process model below.

**(D) Two parallel harnesses (one per language) sharing a spec.**
Mentioned only to reject explicitly: two timing loops that are "specified" to
be identical never are, and every cross-language comparison inherits the
delta. This is the worst option for a benchmark whose entire point is
cross-candidate comparability.

### Recommendation

**Rust harness. Candidate contract defined as a versioned C header. Candidates
statically linked into the harness binary via thin per-candidate glue crates.
The run orchestrator spawns one child process per (candidate, dataset) pair,
so candidates are isolated from each other without any IPC in the timing
path.**

Why each piece:

- **Rust for the harness** because the harness is precisely the code that
  must be boringly correct (it is the trust root: oracle, gate, clock,
  formats), and because cargo gives single-command reproducible builds with a
  lockfile — which is itself a reproducibility requirement. This is *not* a
  claim that Rust candidates are preferred; the contract keeps candidate
  languages symmetric.
- **C ABI as the contract** because it is the only stable, tool-agnostic
  boundary both languages speak natively. The header is the single source of
  truth, versioned (`LB_ABI_VERSION`), and checked at registration. It also
  means the design is already dlopen-ready: if the workspace ever grows a
  candidate whose build shouldn't burden everyone (a giant C++ dependency
  tree), the same contract can be loaded from a `.so` with zero changes to
  candidate code. Static linking first because it is the simplest thing that
  works and keeps everything in one binary with one `cargo build`.
- **In-process calls** because a query against a few hundred MB of strings
  runs from microseconds to milliseconds, and an indirect function call is
  nanoseconds — the integration mechanism vanishes from the measurement.
  One clock, one loop, one warmup policy, for every candidate ever added.
- **Process-per-candidate runs** because in-process was the only real risk in
  this choice: a buggy C++ candidate scribbling over the heap could silently
  corrupt another candidate's measurement, and allocator/cache state could
  bleed between candidates. Spawning a fresh child (the harness re-executing
  itself with `--worker candidate=X dataset=Y`) per candidate gives each one
  a clean address space and lets a segfault fail *one* cell of the run
  matrix loudly instead of the whole run. This captures the isolation benefit
  of option (C) without putting IPC anywhere near the timing path — the child
  mmaps the dataset itself and writes results to a file; the parent only
  aggregates.

What adding a candidate looks like:

- *C++ candidate:* a directory with a `CMakeLists.txt` (author-owned flags,
  e.g. `-march=native`), implementing the functions in `lb_candidate.h`. A
  ~20-line glue crate invokes CMake from `build.rs` (via the `cmake` crate —
  the same battle-tested path used to embed zstd/rocksdb) and registers the
  vtable. The author writes zero Rust; the glue crate is copy-paste.
- *Rust candidate:* a crate implementing the same vtable via `extern "C"`
  shims. The author writes zero C++.
- *Scanner (either language):* the same pattern against the smaller
  `lb_scanner` vtable (§7) — uncompressed eval kernels are pluggable exactly
  like storage candidates.

Contract rules that make in-process safe and fair (enforced by documentation
and review, verified where possible): candidates must not replace the global
allocator, must not spawn threads in phase 1 (see §9), must not retain
pointers into the dataset view beyond `destroy()`, must treat the view as
read-only (the harness maps it read-only, so violations fault loudly), and
must not carry work across `run()` calls — the no-memoization rule detailed
in §7.

---

## 4. Dataset format

### Canonical artifact

Benchmarks never run against raw sources. An **ingest step** (a harness
subcommand) converts an arbitrary source (Parquet/CSV/TSV — ClickBench ships
all three) into a **canonical dataset artifact**, once, deterministically,
cached on disk:

- **`data.arrow`** — an Arrow IPC file containing a single record batch with
  a single `LargeBinary` column, uncompressed, 64-byte-aligned buffers. This
  *is* the prepared in-memory form: one contiguous payload buffer plus one
  `u64`-widths offsets buffer. Load = mmap + validate; there is no
  deserialization step to get wrong or to accidentally vary per candidate.
- **`manifest.json`** — self-description and provenance: dataset id and
  version, source URL/recipe, ingest options, row count, payload byte count,
  min/max/mean string length, byte-frequency summary, and an **xxh3 checksum
  of the logical content**. The checksum is the dataset's identity; query
  truth binds to it (§5), and results record it.

Why Arrow IPC rather than a bespoke flat format: the layout we want (offsets
+ contiguous bytes) *is* Arrow's layout, so we get mmap-ability and alignment
for free, and — more valuable — datasets can be authored, inspected, and
sanity-checked with pyarrow/polars/DuckDB without going through the harness.
The future query generator will almost certainly want to analyze datasets
from Python; a standard format keeps that door open. If arrow-rs's zero-copy
mmap path ever proves awkward, the load path is one function and can fall
back to a single aligned copy at startup — load cost is setup, not
measurement, so the design doesn't depend on the optimization.

Single record batch, `LargeBinary` (64-bit offsets): candidates get exactly
one `(bytes, offsets, num_rows)` triple, with no chunk-handling logic leaking
into every candidate, and columns larger than 2 GB (ClickBench URL-scale)
work without chunking.

### Semantics decisions (normalized at ingest, recorded in the manifest)

- **Strings are byte strings.** No UTF-8 requirement, no case folding, no
  collation. Real columns contain junk bytes, and the SIMD algorithms under
  test operate on bytes; the benchmark should too. (`LargeBinary`, not
  `LargeUtf8`, precisely to make this explicit.)
- **Nulls are removed at ingest** and the removed count is recorded in the
  manifest. SQL `LIKE` on NULL yields NULL (excluded from matches), so
  dropping nulls preserves the interesting semantics while sparing every
  candidate a validity-bitmap code path. Empty strings are kept — they are
  real data and a real edge case. *(Decided in review: drop.)*
- Ingest is deterministic: same source + same options ⇒ byte-identical
  artifact and checksum. Options (null policy, source column) are part of
  the recorded recipe, so any variation is a *different* dataset with a
  different identity. There is no ingest-time size cap — chunking at prepare
  time (§6) is the scale mechanism, and a column genuinely too large for a
  machine is sliced at the source before ingest, which honestly yields a
  distinct dataset. *(Decided in review round 2.)*

---

## 5. Query and suite format

### Shape

A **suite** is a directory: `suite.json` (manifest) + `queries.jsonl` (one
query per line). Two files rather than one so that a generator can stream
queries out without holding a document in memory, diffs stay line-oriented,
and the manifest can be read without scanning queries.

`suite.json` carries: suite id/version, human description, **the dataset id
and checksum the suite is bound to**, provenance (curated-by, or generator
name/version/seed/config), and the truth-hash algorithm version.

Each line of `queries.jsonl`:

```json
{"id": "urls.contains.rare-needle.0042",
 "op": "contains",
 "needles": ["jetbrains"],
 "meta": {"gen": {"axis": "needle_rarity", "target_selectivity": 1e-4,
                   "needle_source": "sampled_from_data", "seed": 991},
          "note": "anything; harness carries it verbatim"},
 "truth": {"count": 1289, "hash": "xxh3:9f31c2...", "algo": "bitmap-xxh3-v1",
            "sample_indices": [17, 402, 995]},
 "derived": {"selectivity": 1.289e-4, "needle_len": 9,
              "rarest_byte_freq": 0.0021}}
```

### The five operations, one representation

Every operation is `op` + an ordered list of byte-string needles:

| `op` | needles | semantics (SQL-LIKE-equivalent) |
|---|---|---|
| `prefix` | exactly 1 | `LIKE 'n%'` — row starts with needle |
| `suffix` | exactly 1 | `LIKE '%n'` — row ends with needle |
| `contains` | exactly 1 | `LIKE '%n%'` — needle occurs anywhere |
| `multi_contains` | ≥ 1, ordered | `LIKE '%a%b%c%'` — needles occur **in order, non-overlapping**: each needle's match begins at or after the end of the previous needle's match |
| `contains_any` | ≥ 1, unordered | `LIKE '%a%' OR LIKE '%b%' OR …` |

Edge-case semantics are part of the contract and implemented by the oracle:
an empty needle matches every row; a needle longer than the row matches
nothing; duplicate needles in `multi_contains` require distinct sequential
occurrences; `contains_any` with duplicate needles is the same as without.
These get a dedicated fixture-based unit-test suite, because the oracle is
the root of trust.

Needles are byte strings. In JSON, a needle is either a plain JSON string
(UTF-8 text — the overwhelmingly common, human-readable case) or
`{"b64": "..."}` for arbitrary bytes. Curated suites stay hand-writable;
binary-junk needles remain expressible.

### How this accommodates the future generator (and curated queries)

The load-bearing choice: **the harness treats only `id`, `op`, `needles`, and
`truth` as semantics.** Everything else is metadata, split into two spaces
with different trust levels:

- **`meta` — declared, opaque, verbatim.** The harness never interprets it;
  it flows unmodified into every result row so downstream analysis can join
  and group by it. The generator writes whatever it swept (axis, target
  selectivity, needle provenance, fragment gaps, seed) under `meta.gen.*`;
  curated queries write a sparse `meta` or none. New generator parameters
  never require a format or harness change — that is the mechanism by which
  the format doesn't preclude the generator.
- **`derived` — computed, trusted, stamped by the harness at bless time.**
  True selectivity (from truth count), needle lengths, needle byte/gram
  frequency *measured against the actual dataset*, match-position summary.
  Analysis of "where does this prefilter collapse" joins candidate stats
  against `derived`, never against hand-written claims. A generator may
  *target* a selectivity in `meta`; the value analysis uses is the measured
  one in `derived`.

Extensibility: `op` is an open string in the format with a closed, versioned
enum in the ABI. A future general pattern op (`'a%b_c'`) or an
op-specific parameter block can be added without breaking existing suites.

### Ground truth

Truth is **`count` + an order-independent hash of the exact match set**,
defined precisely as xxh3-64 over the canonical result bitmap's little-endian
words with padding bits zeroed, and versioned (`bitmap-xxh3-v1`) so the
representation can evolve without silently invalidating stored truths. Full
index lists are not stored (a 50%-selectivity query on 100M rows would be
400 MB of truth); `sample_indices` (first N matches) is stored purely as a
debugging aid. When a gate fails, the harness recomputes the oracle result
live and reports the first divergent row and its bytes — so debugging never
depends on stored indices.

Truth is produced by **`bench bless`**: run the oracle over the suite against
its bound dataset, fill `truth` and `derived`, and stamp the dataset
checksum. A suite whose checksum doesn't match the dataset it's pointed at is
rejected at load — truth is never silently reused across dataset versions.
Curated-query authoring is therefore: write `op` + `needles` (+ optional
`meta`), run bless, commit. The future generator does exactly the same thing,
or computes truth itself and has bless verify it.

---

## 6. The single load-and-prepare path

One module owns the entire journey from disk to candidate input; nothing else
touches files.

1. **Dataset load:** open canonical artifact → mmap read-only → validate
   header/alignment (checksum verification on demand or first-use, cached) →
   produce the one `PreparedDataset`: `{bytes: &[u8], offsets: &[u64],
   num_rows}` plus manifest metadata.
2. **Suite load:** parse `suite.json` + `queries.jsonl` → verify dataset
   binding (checksum) → verify every query has blessed truth (else refuse to
   run, pointing at `bench bless`) → produce `PreparedQuery` list with
   needles decoded to bytes.
3. **Candidate view:** the C-ABI struct handed to `build()` is a direct,
   read-only view of one chunk of the mmap (chunking below) — same pointers
   for every candidate, no per-candidate copies. The layout is deliberately
   Arrow-compatible, so Rust candidates (e.g. Vortex later) can wrap it
   zero-copy into a real Arrow array, and C++ candidates get the two raw
   buffers they'd want anyway.

Every candidate — and the oracle — consumes the identical bytes through this
one path. There is no second loader to drift.

### Chunking: chunk size is a prepare-time parameter, not a dataset property

Real systems compress and scan string columns in blocks — row groups,
per-block dictionaries and symbol tables — and chunk size moves *both* axes
at once: smaller chunks mean more dictionary/symbol-table overhead per byte
(compression) and different cache residency and prefilter granularity
(latency). That makes it a dimension worth sweeping, and it fits the design
as a **prepare-time parameter** in the run spec rather than a property of the
dataset artifact:

- The canonical artifact stays one contiguous column. At prepare time the
  loader slices it into contiguous runs of `chunk_rows` rows (last chunk
  ragged), materializing per-chunk views with rebased offsets — a one-time
  setup cost, outside all measurement. `chunk_rows` must be a multiple of 64
  so each chunk owns whole words of the result bitmap.
- `build()` runs once per chunk; a candidate instance is a vector of chunk
  handles. Footprint and build time are summed across chunks — per-chunk
  dictionary/symbol-table overhead therefore shows up on the compression
  axis, which is exactly the phenomenon worth observing.
- A timed sample for a query = run it over every chunk back-to-back, each
  chunk writing its own aligned slice of the global bitmap. Latency remains
  one number per query; per-row normalizations are unchanged.
- **Truth is chunk-invariant.** Chunking partitions the same logical rows in
  the same order, so the global match bitmap — and its hash — are identical
  at every chunk size. Suites bind to the logical content checksum, so
  **sweeping chunk size never re-blesses**: one `bless`, N chunk sizes.
- `chunk_rows` is recorded on every result row; the compression axis is keyed
  by (candidate, config, dataset, chunk_rows), and sweeping it is just
  multiple entries in the run spec.
- There is no fixed, built-in block size: `chunk_rows` is **user-defined in
  the run spec** (any multiple of 64), and when unspecified the default is a
  single chunk covering the whole dataset. Because truth is chunk-invariant
  and the compression axis is keyed by chunk size, any user-chosen value
  yields well-defined, comparable results — and with ingest caps removed,
  chunking doubles as the scale mechanism. *(Decided in review round 3.)*

---

## 7. The candidate contract

### Lifecycle

```
describe ──► per chunk: build(chunk_view, config) ──► footprint(handle)
                                   │
                                   ├──► run(handle, strategy, query, out_bitmap, stats?)
                                   │        [candidate-implemented strategies, e.g. compressed]
                                   ├──► view(handle) ──► chunk_view      [uncompressed: enables direct]
                                   ├──► decode(handle, out_bufs)         [compressed: enables decode]
                                   └──► destroy(handle)
```

- **`describe`** — name, version string, ABI version, and any
  **candidate-implemented strategies** (next subsection), e.g. `compressed`,
  each with its own bitmask of supported ops. May be empty: a scheme whose
  only path is decode-then-scan implements no `run` at all. Unsupported
  (candidate, strategy, op) cells are recorded as *unsupported* in results,
  not failures — a scheme whose compressed-domain path only does `contains`
  is a legitimate candidate.
- **`build`** — consume one chunk view, construct the candidate's internal
  representation for that chunk (this *is* the compression step; one handle
  per chunk, §6). Harness-timed; build/compress cost is the summed time
  across chunks. Takes an opaque config string (JSON) so one implementation
  can expose variants (dictionary size, prefilter width); each (candidate,
  config) pair is a distinct row in results, identified by name + config
  hash.
- **`footprint`** — report resident bytes as **named components**, e.g.
  `{"payload": …, "offsets": …, "prefilter": …}`. Named, because prefilters
  cost space too, and attributing prefilter storage is part of treating
  prefiltering as first-class: the compression axis and the prefilter axis
  meet here. Self-reported and trusted, subject to code review *(decided in
  review round 3)*; a warn-level cross-check against the worker's RSS delta
  across `build()` remains a cheap later addition if ever warranted.
- **`run`** *(NULL when no strategies are declared)* — answer one query
  *under a declared strategy* by setting bits in the harness-provided,
  zeroed bitmap slice for this handle's chunk. The `stats` pointer is NULL
  in timing mode (§9) and non-NULL in instrumented mode (§10).
- **`view`** *(optional)* — expose this handle's stored data as a canonical
  chunk view, zero-copy, when the scheme stores it uncompressed. Enables the
  harness-composed `direct` strategy.
- **`decode`** *(optional)* — decompress this handle's chunk into
  caller-provided buffers in the canonical layout. Enables the
  harness-composed `decode` strategy (next subsection).
- **`destroy`** — release everything.

A candidate must offer at least one way to answer: `run` (with declared
strategies), `view`, or `decode`.

### Execution strategies: one build, several ways to answer

A storage scheme and the way it answers a query are separable concerns.
FSST- or OnPair-compressed data can be prefiltered and matched **directly in
the compressed domain**, or **decompressed and run through a common
uncompressed prefilter + eval pipeline**. Both are operating points *of the
same built representation*, with the same footprint and build cost — one
compression-axis point carrying several latency curves. The contract
distinguishes three kinds of strategy, and only the first is
candidate-implemented:

- **`compressed`** (and custom-named hybrids, e.g. partial decode):
  candidate-implemented via `run()` — match/prefilter directly on the
  compressed representation.
- **`decode`**: harness-composed — the candidate's `decode()` fills a
  pre-allocated scratch chunk, then a **scanner** (next subsection) runs the
  uncompressed prefilter + eval over it. This is *the* decode path:
  candidates never hand-roll their own decode-then-eval pipeline *(decided
  in review round 2 — every current algorithm follows decompress → common
  uncompressed pipeline)*. A genuinely fused decode+scan optimization, if
  one ever appears, is a custom strategy via `run()`.
- **`direct`**: harness-composed — a scanner runs straight over the
  candidate's `view()`, zero-copy, when the stored form is already the
  canonical layout. This is `decode` minus the decode cost, and it is how
  uncompressed schemes are scanned.

Because `direct` and `decode` are parameterized by scanner, their result
rows are keyed (candidate, strategy, **scanner**, …); `compressed` rows
carry no scanner. The cross-product answers two families of questions at
once: for a compressed scheme, "in-domain matching vs. decode-then-scan, per
needle length / selectivity / rarity, at identical footprint"; and across
scanners, "which uncompressed eval wins in which region" — over raw data via
`direct` and over freshly decoded data via `decode`.

**No memoization across calls.** A `decode`-strategy run pays the full
decode on every call — the steady-state model is data at rest compressed,
with every query paying the read path. Scratch *allocations* may persist
across calls (pre-allocated buffers are steady-state-realistic, and per-call
multi-MB mallocs would measure the allocator); scratch *contents* must not
carry information between calls. This applies to every strategy and joins
the candidate hygiene rules in §3.

### Scanners: a first-class registry of uncompressed eval functions

The common uncompressed pipeline is deliberately **not one blessed
implementation but a registry of them** — the benchmark evaluates
uncompressed eval functions as subjects in their own right, not as a fixed
harness detail. Phase 1 seeds the registry with a memmem-based scanner
(Rust) and a std-find scanner (C++); natural successors include a
naive-scalar lower bound, Teddy-style multi-pattern SIMD prefilters, and
other SIMD-prefilter+verify designs. Each lands as one more registry entry
that automatically applies to *every* candidate's `direct` and `decode`
strategies — add one scanner, and every storage scheme gains a new measured
combination.

A scanner is a small C ABI, pluggable from either language with the same
glue-crate pattern as candidates:

- **`prepare(query)`** — compile the needles (masks, tables, automata).
  Chunk-independent, so it runs once per query rather than once per chunk,
  and its cost is included once in each timed sample — it is part of
  answering the query.
- **`scan(prepared, chunk_view, out_bitmap, stats?)`** — the per-chunk
  prefilter + eval; fills the same `lb_run_stats` as candidates in
  instrumented mode.
- **`release(prepared)`**.

Scanners declare supported ops, and the run spec selects them exactly like
candidates; unsupported or platform-unavailable combinations surface as
recorded cells. The correctness oracle stays entirely independent of every
scanner — scanners are measured subjects, the oracle is the judge.

### Illustrative ABI sketch

Non-final, but concrete enough to evaluate the shape:

```c
/* contract/lb_candidate.h — the single source of truth, versioned */
#define LB_ABI_VERSION 1

typedef struct { const uint8_t* ptr; uint64_t len; } lb_bytes;

typedef struct {
  const uint8_t*  bytes;     /* chunk payload, read-only                */
  const uint64_t* offsets;   /* num_rows + 1 entries, rebased to chunk  */
  uint64_t        num_rows;  /* row i = bytes[offsets[i]..offsets[i+1]) */
} lb_chunk_view;

typedef enum { LB_PREFIX, LB_SUFFIX, LB_CONTAINS,
               LB_MULTI_CONTAINS, LB_CONTAINS_ANY } lb_op;

typedef struct {
  lb_op           op;
  const lb_bytes* needles;
  uint32_t        needle_count;
} lb_query;

typedef struct {
  const char* name;          /* "compressed" | custom; "direct"/"decode"
                                are harness-composed, never declared here  */
  uint32_t    supported_ops; /* bitmask over lb_op                          */
} lb_strategy;

#define LB_STAT_UNSET UINT64_MAX
typedef struct {                     /* instrumented mode only          */
  uint64_t prefilter_candidates;     /* rows surviving prefilter        */
  uint64_t decode_ns;                /* self-timed phase breakdown      */
  uint64_t prefilter_ns;             /* (all fields optional: UNSET)    */
  uint64_t verify_ns;
} lb_run_stats;

typedef struct {
  uint32_t           abi_version;
  const char*        name;
  const char*        version;
  const char*        cpu_features;   /* required host features, e.g.
                                        "avx2,bmi2" / "avx512f,avx512bw" /
                                        "neon"; NULL = portable. Unmet =>
                                        module never runs on this host (§9) */
  const lb_strategy* strategies;     /* candidate-implemented; may be 0  */
  uint32_t           strategy_count;
  void* (*build)(const lb_chunk_view*, const char* config_json,
                 char* err_buf, uint64_t err_cap);
  uint32_t (*footprint)(void* self, lb_footprint_component* out,
                        uint32_t capacity);
  int   (*run)(void* self, uint32_t strategy, const lb_query*,
               uint64_t* out_bitmap_words, lb_run_stats* stats_or_null);
                                     /* NULL iff strategy_count == 0     */
  /* optional: zero-copy view of stored data already in canonical layout
     — enables the harness-composed "direct" strategy                    */
  int   (*view)(void* self, lb_chunk_view* out);
  /* optional: decompress this chunk into caller buffers, canonical
     layout — enables the harness-composed "decode" strategy             */
  int   (*decode)(void* self, uint8_t* bytes_out, uint64_t bytes_cap,
                  uint64_t* offsets_out);
  void  (*destroy)(void* self);
} lb_candidate;   /* must offer at least one of: run, view, decode       */

typedef struct {
  uint32_t    abi_version;
  const char* name;
  const char* version;
  const char* cpu_features;          /* same hard gating as candidates   */
  uint32_t    supported_ops;         /* bitmask over lb_op               */
  void* (*prepare)(const lb_query*); /* needle compilation, per query    */
  int   (*scan)(void* prepared, const lb_chunk_view*,
                uint64_t* out_bitmap_words, lb_run_stats* stats_or_null);
  void  (*release)(void* prepared);
} lb_scanner;
```

### Why a bitmap output

The result contract is: candidate sets bit *i* iff row *i* matches, in a
harness-owned, pre-zeroed bitmap. Chosen over an index-list buffer because it
is fixed-size (num_rows/8 bytes — 12.5 MB per 100M rows, vs. up to 800 MB for
a worst-case u64 index list), order-free (no "must be sorted" clause to
verify), cheap to hash canonically, and uniform for every candidate at every
selectivity — so the small cost of writing output is identical across
candidates and included in latency for all of them equally. For very sparse
results an index list can be marginally faster to emit; that difference is
noise relative to the scan itself, and uniformity wins. *(Confirmed in
review.)*

### Registration

A static registry in the harness: each glue crate exposes its `lb_candidate`
vtable; a feature flag per candidate controls what gets compiled in. Adding a
candidate = add directory + one registry line. What actually *runs* is chosen
by a **run spec** file (`spec.toml`: candidates + configs × scanners × datasets ×
chunk sizes × suites), which is itself hashed into the results manifest — a
run is reproducible from its spec. Scanners register identically to
candidates, via their `lb_scanner` vtable behind a feature flag.

---

## 8. Correctness gating

- **The oracle** lives in the harness: naive, allocation-free byte loops for
  the five ops. Deliberately dumb — no memchr, no SIMD — because the
  baseline candidate will use `memchr`-family SIMD kernels, and the oracle
  must be *independent* of every candidate's machinery: a shared memmem bug
  must not be able to pass the gate. The oracle is the most-reviewed,
  most-tested code in the project: exhaustive fixture tests over the edge
  cases in §5, plus randomized differential tests against a second trivial
  implementation (e.g. Rust std `windows()`-based) in CI.
- **Bless** caches oracle output as truth in the suite, bound to the dataset
  checksum (§5).
- **The runtime gate:** for every result cell (candidate × strategy ×
  scanner × query), the *first* run is a verification pass — the harness hashes the produced bitmap and compares
  (count, hash) against truth. Only after a query passes does the timing loop
  run, and timing iterations are not re-verified (verification cost stays out
  of latency; the candidate cannot detect verification vs. timing runs since
  the call is identical in timing mode).
- **Failure is loud and sticky:** a mismatch marks that (candidate, query)
  cell failed, reports count-diff and the first divergent row index with its
  bytes (recomputed live from the oracle), withholds all latency/compression
  numbers for that cell, and forces a nonzero exit for the whole run.
  `--fail-fast` aborts at first mismatch for debugging sessions.
- **The gate is itself tested:** the test suite includes a deliberately wrong
  candidate (off-by-one on row 0) and asserts the run fails. A gate that has
  never fired is not known to work.

---

## 9. Measurement methodology

### The two axes, kept separate

**Compression axis** (per candidate × config × dataset × chunk size; measured
once per build, outside any query loop — strategies share it, since every
strategy answers queries from the same build):

- `build_ns` — harness-timed wall time of `build()`. This is the
  compress-cost number.
- `footprint_bytes` — sum of the named components across chunks, plus each
  component individually.
- Ratio is *derived at reporting time*, never stored as primary data:
  `raw_bytes / footprint_total`, where `raw_bytes` is the canonical view's
  size (`payload + 8·(num_rows+1)`), so the uncompressed baseline sits at
  ≈1.0 by construction. Results store absolute bytes so any alternative
  ratio definition remains derivable later.

**Latency axis** (per candidate × config × strategy × scanner (for
`direct`/`decode`) × dataset × chunk size × query; one timed sample = needle
prepare + the query over all chunks, §6):

- Warmup runs (default 3), then samples until both a minimum iteration count
  and a minimum time budget are met (defaults: ≥10 iterations and ≥200 ms,
  adaptive so microsecond queries get thousands of samples and 100 ms queries
  get a sane few).
- Stored: sample count, min / p25 / median / p75 / p99 / max / mean / stddev;
  full raw samples behind a `--raw` flag for distribution analysis.
  Headline number: **median**, with min reported alongside as the noise
  floor and the stored distribution (p99 etc.) there for comparing tails
  across candidates. *(Confirmed in review.)*
- Derived per-row normalizations stored with each result: `ns_per_row` and
  effective bytes/s over the *raw* payload — these make cross-dataset
  comparison meaningful regardless of each candidate's compression.
- Clock: monotonic (`Instant`); the timing loop is ~30 lines used by every
  candidate identically. Hardware counters (instructions, cache misses via
  `perf_event`) are a natural later extension of the same loop, not a
  redesign.

### Run hygiene and reproducibility

- One child process per (candidate, dataset): fresh address space, fresh
  allocator state, candidate crash = one failed matrix cell.
- The worker pins itself to a core; the harness records (and warns on) CPU
  governor, and captures: CPU model and flags, core count, OS/kernel,
  rustc/cc versions and flags, git commit + dirty bit, dataset and suite
  checksums, spec hash, timestamp.
- Output: `results.jsonl` — one self-contained row per (candidate, config,
  strategy, scanner, dataset, chunk size, query) carrying the query's
  `meta`/`derived` verbatim plus all measurements — and `manifest.json` with
  the environment capture. Results
  are pure data; reporting/plots (out of scope) consume them later without
  re-running anything.
- Hot-cache latency is the phase-1 definition (data resident, candidate
  built). Cold-start behavior is a future dimension, noted, not designed here.

### Threading policy (a decision, not an accident)

Phase 1 defines latency as **single-threaded, core-pinned** latency, and the
contract forbids candidates spawning threads. Single-core numbers are the
comparable, interpretable primitive that per-algorithm analysis needs;
parallel scaling is a separate later dimension (it would become an explicit
`threads` parameter in the run spec, measured as such, for all candidates
uniformly). Letting each candidate freelance on threads would silently turn
the latency axis into a scheduler benchmark. *(Confirmed in review.)*

### Platform policy

The measurement target is **x86-64 Linux with AVX2/AVX-512**; published
numbers come from there. **arm64 macOS (Apple Silicon) is a first-class
development platform**: the harness, formats, oracle, gate, and baseline
(whose memchr kernels have NEON paths) all build and run locally, so the full
workflow — ingest, generate, bless, run, analyze — works on a MacBook.
Candidates and scanners with ISA-specific kernels declare their requirements
in the vtable (`cpu_features`, e.g. `"avx2"` or `"avx512f,avx512bw"`). At
startup the harness detects the host's features and **hard-gates** every
module whose requirements are unmet: it is never called, and its cells are
recorded as *unavailable on this platform* (same mechanism as unsupported
ops) — never a silent absence. There is deliberately **no scalar-fallback
requirement**: a SIMD candidate silently downgrading to scalar code would
produce numbers that look comparable and aren't. When a scalar variant is
worth measuring, it is registered as its own explicit candidate/scanner (or
config), so every result row states exactly which kernel ran. *(Decided in
review round 3.)* Every results manifest records CPU model, ISA features, and
platform, and rows from different machines are never merged into one
comparison — cross-machine analysis compares manifests side by side,
explicitly.

---

## 10. Prefiltering as a first-class, inspectable concern

The design goal: attribute *why* a candidate is fast or slow — specifically,
how much its prefilter prunes and what survivors cost — without ever
polluting the latency numbers with instrumentation.

**Two-mode running.** Every (candidate, query) is executed in both modes by
the same entry point:

- **Timing mode** (`stats == NULL`): the candidate does no bookkeeping; all
  latency samples come from this mode.
- **Instrumented mode** (`stats != NULL`, run once per query *per strategy*,
  outside the timing loop): the candidate fills whatever it can of
  `lb_run_stats` — minimally `prefilter_candidates` (rows surviving its
  prefilter), optionally self-timed `decode_ns` / `prefilter_ns` /
  `verify_ns` phase splits. Every field defaults to UNSET; a candidate with
  no prefilter reports nothing and appears as such.

The two prefiltering kinds map onto strategies (§7), and their attribution
differs in trust level. Under `compressed`, prefiltering happens inside the
candidate's compressed domain, so its counters and phase timings are
necessarily self-instrumented. Under the harness-composed `direct` and
`decode`, the harness owns the pipeline joints: decode time is measured
directly by the harness clock, and prefilter counters come from the scanner
— the same scanner code for every candidate it is paired with, so those
counters are comparable across candidates by construction. Result rows look
identical either way; phase splits are labeled by origin.

**Derived attribution — computed by the harness, not trusted from the
candidate.** From `prefilter_candidates` plus the (gated, therefore true)
match count, the harness derives per query:

- prune rate: `1 − candidates/num_rows`
- false-positive rate of the prefilter: `(candidates − true_matches) / candidates`
- verify cost per survivor: timing-mode latency divided by candidates
  (an honest approximation; exact splits come from the optional self-timed
  phases, clearly labeled as self-reported)

**Where the insight comes from:** every result row carries both these
prefilter metrics and the query's `derived` metadata (true selectivity,
needle length, needle rarity measured against the dataset). "This prefilter
is superb below 10⁻³ selectivity and collapses to a 98% false-positive rate
when the needle contains a top-decile byte" is then a *join and a group-by
over results*, not a new measurement campaign — and when the parameterized
generator arrives, its swept axes land in the same joinable place. The
footprint components (§7) complete the picture: a prefilter's storage cost
sits on the compression axis of the same result rows.

Self-timed phase splits are the one place the contract accepts numbers from
candidates. They are optional, labeled self-reported in results, and never
mixed into the headline latency (which is always harness-timed, timing-mode).
For the harness-composed strategies the decode/scan boundary is timed by the
harness directly; only the scanner's internal prefilter counters are
plugin-reported, and they come from shared scanner code rather than from the
candidate being measured.

---

## 11. Repository layout and phase-1 plan

```
contract/        lb_candidate.h + SEMANTICS.md (op definitions, edge cases,
                 candidate rules) — the cross-language source of truth
harness/         Rust crate: CLI (ingest, bless, run, check), formats,
                 loader, oracle, timing, gate, results
candidates/
  uncompressed/  Rust: trivial storage candidate — retains the chunk view,
                 exposes view(); all scan smarts live in scanners
  cpp_identity/  C++: smoke candidate — copies chunks, decode() = memcpy;
                 proves the C++ build/decode path end-to-end
  onpair/        C++: OnPair (pinned via FetchContent) — the first real
                 compressed candidate; decode() plus a "compressed"
                 strategy (token automata over the packed stream)
  onpair_spiral/ Rust: SpiralDB's OnPair (git dep, github.com/spiraldb/onpair,
                 branch feat/search-prefilter) — three contains-only run()
                 strategies: "pf_kmp" (SIMD prefilter + compressed-domain
                 token-KMP verify), "pf_memmem" (prefilter + decode-survivors +
                 memmem verify), and "kmp" (token-KMP over every row, no
                 prefilter — the cross-library counterpart of `onpair`'s
                 `compressed`); a distinct library/scheme from `onpair`
scanners/
  memmem/        Rust: memchr/memmem SIMD kernels (phase-1 workhorse)
  cpp_std_find/  C++: std::string_view::find — proves the C++ scanner path
datasets/        manifests + fetch/ingest recipes (artifacts gitignored)
suites/          curated suites, checked in (queries.jsonl + suite.json)
```

### The vertical slice, in order

1. **Contract first:** `lb_candidate.h` + `SEMANTICS.md` with the op
   definitions and edge-case table from §5, the reserved strategy names and
   no-memoization rule from §7, and the candidate hygiene rules from §3.
2. **Oracle + its tests** — hand-built fixtures covering every edge case,
   differential tests. The trust root exists before anything it will judge.
3. **Dataset path:** `ingest` (Parquet/CSV → canonical artifact + manifest),
   mmap loader, checksums. First dataset: a ClickBench column (proposed:
   `URL` from hits — realistic skew; plus a small checked-in fixture dataset
   for tests).
4. **Suite path:** parse, validate, `bless`.
5. **Runner:** process-per-candidate orchestration, chunk loop, strategy ×
   scanner loops, timing loop, two-mode execution, gate, `results.jsonl` +
   manifest.
   Chunked runs are exercised in tests (small fixture dataset at several
   chunk sizes must produce identical gated results), even though the
   phase-1 default is a single chunk.
6. **Uncompressed candidate + first scanners:** the storage candidate is
   deliberately trivial — build retains the chunk view, `view()` exposes it,
   footprint = raw (ratio 1.0 by construction), no `run()`. The scan smarts
   live in scanners: `memmem` (Rust — `memchr::memmem` for `contains`, a
   genuinely strong SIMD substring kernel; bounds-checked `memcmp` for
   prefix/suffix; sequential memmem with position advance for
   `multi_contains`; first-match-wins loop for `contains_any`;
   Aho-Corasick/Teddy remain *future scanners*) and `cpp_std_find` (C++).
   The combination (uncompressed × memmem × `direct`) is the brief's
   reference baseline. Neither phase-1 scanner reports prefilter stats —
   they have no prefilter.
7. **C++ identity smoke candidate:** `build` copies the chunk into private
   buffers (a "compression" scheme at ratio ≈1), exposes `decode()` (memcpy
   back out), no `run()`. Deliberately tiny, yet it exercises exactly the
   shape every real compressed candidate will use — C++ built under cargo,
   named footprint components, and the composed `decode` strategy across
   every scanner — so the risky machinery is proven before FSST or OnPair
   arrive. *(Confirmed in review: kept, now in identity-codec form.)*
8. **End-to-end + gate test:** a curated ~25-query suite over the first
   dataset covering all five ops and the edge cases; the deliberately-wrong
   candidate proving the gate fires; a full run producing gated results.

**Definition of done:** one command each for ingest → bless → run, from raw
ClickBench file to `results.jsonl`, all cells gated; injected wrongness fails
loudly; adding a mock candidate — or scanner — in either language touches
only its own directory plus one registry line.

### Deliberate divergences from the brief's hints, consolidated

- Truth is count + set-hash (with debug samples), not a stored row set — the
  brief allowed either; stored full sets don't scale with high selectivity.
- The correctness reference lives in the harness as the oracle, not as a
  candidate — it must be independent of candidate machinery to be a root of
  trust.
- Neither pure in-process FFI nor subprocess IPC, but in-process timing with
  process-per-candidate isolation — captures the fidelity of the former and
  the isolation of the latter.
- Instrumentation is a separate execution mode, so "inspect prefiltering" and
  "trust the latency numbers" never trade off against each other.
- A second (trivial C++) candidate added to phase 1 to de-risk the bilingual
  contract immediately.
- *(Implementation, 2026-07-07)* The deliberately-wrong candidate became a
  visible registered candidate, `gate_canary`, with two strategies: `ok`
  (correct naive matcher — also phase 1's only exerciser of the
  candidate-implemented `run()` path) and `wrong` (bit-flip on row 0). A
  dedicated spec (`specs/gate-canary.toml`) must exit 3; the integration
  suite asserts both strategies.
- *(Implementation, 2026-07-07)* The loader currently reads the IPC file
  into aligned buffers (one copy at startup) rather than zero-copy mmap —
  the fallback §4 explicitly allows; load cost is setup, not measurement,
  and the swap remains a one-function change.
- *(Implementation, 2026-07-07, ABI v2)* `decode()` output buffers carry a
  guaranteed `LB_DECODE_PAD` (64 B) of writable headroom past the payload
  (SEMANTICS.md rule 8). Fixed-stride over-copying decoders (OnPair emits a
  constant `MAX_TOKEN_SIZE` memcpy per token; FSST-style codecs do the same
  per symbol) would otherwise need a private buffer plus a full defensive
  memcpy inside the timed decode path — precisely the allocation/copy
  pollution the measurement discipline exists to keep out.
- *(Implementation, 2026-07-07)* First real compressed candidate: `onpair`
  (github.com/gargiulofrancesco/onpair_cpp, FetchContent pinned to a
  commit; `ONPAIR_SOURCE_DIR` overrides with a local checkout). It is also
  the first exerciser of decision 9's two prefilter modes over one build:
  harness-composed `decode`, plus a candidate `run()` strategy
  `"compressed"` supporting prefix/contains/contains_any via token automata
  compiled per call against each chunk's dictionary (suffix and
  multi_contains are not expressible compressed-domain and are declared
  unsupported). Chunk payloads are capped at 4 GiB by OnPair's uint32
  offsets; build() rejects larger chunks with a chunk_rows hint.
- *(Implementation, 2026-07-14)* `onpair_spiral`: SpiralDB's OnPair (the
  Rust library, github.com/spiraldb/onpair) as a query-axis candidate,
  pinned to a rev of its `feat/search-prefilter` branch. A pure-Rust glue
  crate (like lz4/zstd), distinct from the C++ `onpair` — different library,
  different resident form (codes are one u16 per token, not bit-packed). One
  build exposes three contains-only `run()` strategies. Two ride the library's
  SIMD substring prefilter, differing only in the verify step: `pf_kmp`
  (compressed-domain token-KMP over each survivor's codes;
  `ColumnView::rows_containing_prefiltered`, capped at 255-byte needles — a
  longer needle errors the cell) and `pf_memmem` (decode each survivor and run
  memmem; `rows_containing_prefiltered_memmem`, no cap). The third, `kmp`
  (`ColumnView::rows_containing`, same 255-byte cap), runs the token-KMP
  automaton over every row with no prefilter — the un-prefiltered baseline and
  the cross-library counterpart of the C++ `onpair` candidate's `compressed`
  path, so `kmp` vs `compressed` races the two libraries and `pf_kmp` vs `kmp`
  isolates the prefilter's payoff. All three call the
  library's own convenience recipes verbatim, so the benchmark measures what
  OnPair ships; no `lb_run_stats` are filled (filling
  `prefilter_candidates` would mean re-implementing the prefilter/verify
  split, diverging from the shipped method). The prefilter's resident cost
  (its stored `cum_token_freq` per-token frequency prefix sums) is attributed
  on the compression axis as the `prefilter` footprint component — the first
  candidate to exercise §7's named prefilter-storage accounting. Same 4-GiB
  u32-offset chunk cap as `onpair`. Query-axis specs:
  `specs/compression/onpair_spiral_query.toml` (pf_kmp vs pf_memmem vs kmp,
  with onpair `compressed` as the cross-library reference) and the unified
  `specs/shootout/candidates.toml` head-to-head.

---

## 12. Decisions locked in review (rounds 1–3, 2026-07-07)

1. **Nulls:** dropped at ingest; removed count recorded in the manifest.
2. **Dataset scale:** no ingest-time caps (revised in round 2 — chunking
   makes them redundant). Chunk size is a prepare-time, sweepable benchmark
   dimension (§6), not a dataset property; a column genuinely too large for
   a machine is sliced at the source, which is honestly a different dataset.
3. **Threading:** single-threaded, core-pinned latency is the phase-1
   definition.
4. **C++ smoke candidate:** kept in phase 1.
5. **Output contract:** bitmap, confirmed.
6. **Headline statistic:** median; the full distribution (p25–p99, optional
   raw samples) is stored for tail comparisons across candidates.
7. **Platforms:** x86-64 (AVX2/AVX-512) is the measurement target; arm64
   macOS is a first-class local/dev platform (§9 platform policy).
8. **Matching:** byte-exact only; no case folding anywhere in phase 1.
9. **Prefiltering kinds:** two supported modes — compressed-domain
   prefiltering, and decompress → uncompressed prefilter → uncompressed
   eval — modeled as execution strategies over a single build (§7), so a
   compressed candidate can expose both paths at one compression-axis point.
10. **The decode path is always harness-composed.** Candidates supply
    `decode()` (or `view()` when stored uncompressed) and never hand-roll a
    decode-then-eval pipeline; custom fused strategies remain possible via
    `run()`, but no current algorithm needs one.
11. **Scanners are first-class.** The uncompressed eval functions form a
    pluggable registry (§7), benchmarked both standalone (`direct`) and as
    the scan half of `decode` — subjects of the benchmark, not a fixed
    harness detail.
12. **Footprint stays self-reported** for phase 1 — trust + code review;
    the warn-level RSS-delta cross-check remains available as a later
    addition. *(Round 3.)*
13. **No fixed block size.** `chunk_rows` is user-defined per run spec and
    sweepable; when unspecified, the default is one chunk over the whole
    column. Truth chunk-invariance (§6) is what makes arbitrary user-chosen
    sizes safe — no re-blessing, comparable results keyed by chunk size.
    *(Round 3.)*
14. **Phase-1 scanner set:** `memmem` (Rust) and `cpp_std_find` (C++); the
    registry grows from phase 2 onward. *(Round 3.)*
15. **Hard capability gating, no scalar fallbacks.** Candidates and scanners
    declare required CPU features in the vtable (`cpu_features`); the
    harness detects host features at startup and refuses to run any module
    whose requirements are unmet, recording its cells as *unavailable on
    this platform*. Scalar variants, when wanted, are separate explicit
    registrations — never a silent downgrade. *(Round 3.)*

## 13. Open questions

None — the round-2 questions (footprint verification, default chunk size,
phase-1 scanner set, portability floor) were all resolved in round 3 and are
recorded as decisions 12–15 in §12. The design is awaiting approval.

---

## 14. The query generator (`bench gen`)

Status: **approved (review round 4, 2026-07-08); implemented** —
`harness/src/gen.rs` + the `gen` subcommand; scope decisions from that
round are recorded at the end of this section.

### Purpose and paper framing

The generator turns the benchmark from 18 curated points into a systematic
sweep of the parameter space, so that per-candidate behavior can be mapped —
"where does OnPair's compressed-domain execution win, where does it lose,
and by how much" — with the coverage and reproducibility standards of a
VLDB/SIGMOD experimental section. The suite format (§5) was designed for
this client; nothing in the harness, ABI, or results format changes.

### The parameter space, derived from cost models

Axes are not chosen by intuition; they are the union of the variables in the
cost models of the three strategy families the harness executes. A reviewer
asking "why these parameters?" gets this table as the answer.

- **direct** (uncompressed scan):
  `bytes_scanned × f(prefilter false-positive rate) + candidates × verify(needle_len) + matches × output`
- **decode** (decompress → scan):
  `decode(data, config) + direct(...)` — decode is query-independent.
- **compressed** (token-domain automata, OnPair-style):
  `automaton_build(dict_entries × needle_bytes) + tokens × O(1) + matches × output`

Union of query-controlled variables → the generated axes:

| axis | values (full profile) | why (which cost term) |
|---|---|---|
| `op` | all five | structural; each op has its own grid below |
| target selectivity | 0, 10⁻⁵, 10⁻⁴, 10⁻³, 10⁻², 10⁻¹, 0.3, 0.5, 0.8 | matches×output everywhere; callback density compressed-domain; verify density direct |
| needle length (bytes) | 1, 2, 4, 8, 16, 32, 64 (+128 prefix-only) | verify cost direct; automaton states/build compressed; SIMD prefilter width |
| needle count k (`contains_any`) | 2, 4, 8, 16, 64 | Aho-Corasick size vs per-needle rescan; Teddy-class scanners cap near 8 |
| mix profile (`contains_any`) | balanced, skewed (1 common + k−1 rare) | short-circuit behavior: union dominated by one needle vs genuinely multi |
| fragment count f (`multi_contains`) | 2, 3, 4, 8 | per-fragment restart cost; gap-skipping behavior |
| replicates R | 5 (single-needle ops), 3 (multi-needle ops) | separates needle idiosyncrasy from cell effect; error bands in figures |

Two further variables are deliberately **measured covariates, not generated
axes**, because they cannot be controlled independently of selectivity
without synthetic needles: **gram rarity** (`derived.rarest_byte_freq`, plus
a new `rarest_gram2_freq` — the better predictor of SIMD prefilter false
positives) and **match position** (new `derived.match_pos`: mean first-match
byte offset over matching rows — the early-exit variable). Bless stamps
both; analysis regresses against them. The axes that live *outside* the
generator complete the space: dataset (column statistics), candidate config
(e.g. OnPair `bits`), chunk size, scanner — all already swept by run specs.

Needle **provenance is sampled-from-data only** in v1: windows cut from
actual rows, so byte distributions are realistic by construction. The
no-match band uses sampled windows with seeded byte mutations re-verified
absent — realistic statistics, guaranteed zero selectivity — rather than
random garbage. Adversarial classes (periodic needles, common-first-byte
prefilter killers) are a possible later provenance value under the same
format, not phase-2 scope.

### Per-op grids

| op | lengths | selectivity bands | extra axes | R | grid points |
|---|---|---|---|---|---|
| `contains` | 7 | 9 | — | 5 | 315 |
| `prefix` | 8 | 9 | — | 5 | 360 |
| `suffix` | 7 | 9 | — | 5 | 315 |
| `multi_contains` | total ∈ {8, 16, 32} | targeted via fragment rarity, binned | f ∈ {2,3,4,8} | 3 | 180 |
| `contains_any` | per-needle 8 (fixed) | {10⁻⁴, 10⁻³, 10⁻², 10⁻¹, 0.5} | k × mix | 3 | 150 |

~1,320 targets; expected fill after feasibility ~50–70% (see acceptance
below) → **roughly 700–900 queries per dataset**. Many cells are genuinely
infeasible in a given column (a 64-byte needle with 50% selectivity exists
only in data with heavy duplication) — infeasibility is a *finding about the
dataset*, disclosed, never silently skipped.

Generation recipes per op: `contains` — random window of length L from a
random row with `len ≥ L` (guarantees ≥1 match); `prefix`/`suffix` — the
head/tail L bytes of a sampled row (URL prefix trees give a natural
selectivity ladder by depth); `multi_contains` — cut one sampled window into
f ordered fragments with random gaps (witness row guarantees a match;
reversed fragment order supplies the no-match analog); `contains_any` — k
independently sampled windows whose individual probed selectivities follow
the mix profile.

### Targeting, acceptance, and honesty about achieved values

For each grid point the generator draws candidate needles (budget C = 32 per
point), probes each with **the oracle itself** (single root of trust — no
fast-scanner shortcut that could bin-drift), and accepts a candidate whose
measured selectivity lands in the band: within ±0.25 decades of a log-scale
target, within ±20% relative for the dense bands (0.3/0.5/0.8), exactly 0
for the no-match band. Accepted needles are deduplicated globally per op. A
point that exhausts its budget before filling R replicates is recorded as
partially filled with the reason.

Targets live in `meta.gen.*`; truth and achieved values are stamped by the
normal `bless` step, and **analysis reads only `derived`** (§5) — the
generator's own probe results are discarded, so bless remains the single
truth authority at the cost of one redundant oracle pass.

Probe cost at 1M rows: ~1.3k points × ≤32 probes × oracle scan — minutes.
At 100M rows, the same design adds a sample-first stage (probe on a seeded
1M-row sample, exact-verify only acceptances); noted, not built now.

### Determinism and provenance

One u64 seed drives a named PRNG (`splitmix64` → `xoshiro256**`); given the
same dataset artifact (checksum-bound), same generator version, and same
seed, `bench gen` emits **byte-identical** `queries.jsonl`. The grid itself
is code, versioned by the generator version string (`gen1`); changing any
axis value bumps the version. The CLI exposes only narrowing knobs, so a
version+seed pair always names one exact suite:

```
bench gen --dataset datasets/clickbench-url-1m \
    --out suites/clickbench-url-1m-gen1-s42 \
    --seed 42 [--ops contains,prefix] [--profile full|quick]
```

`quick` (iteration profile): lengths {2, 8, 32}, decade bands only, R = 2 —
small enough to regenerate and re-bless in seconds while developing.

Suite artifacts: `suite.json` provenance carries
`{generator: "gen1", seed, profile, ops}`; query ids are grid-readable
(`<suite>.contains.L8.s1e-3.r2`); `meta.gen` carries the full axis point
(op, target band, target length, k/f, mix, witness row index, budget spent).
A third file, **`gen-report.json`**, records the coverage matrix: every grid
point → filled n/R, achieved-value summary, or the reason unfilled
(`no_candidates_in_band`, `length_infeasible`, `budget_exhausted`). Absent
queries cannot self-report; the coverage matrix is how the paper states
what the sweep did *not* cover. All three files are committed.

### Additions to `derived` (bless)

`rarest_gram2_freq` and `match_pos` (mean/median first-match offset among
matching rows), computed during the truth pass at negligible cost. Additive
and backwards-compatible: re-blessing an existing suite gains the fields;
truth verification is unchanged.

### Phase instrumentation: `setup_ns` (ABI v3, additive)

The loss-region explanations need the compressed-domain split between
per-query setup (automaton compilation, ∝ dictionary entries × needle
bytes) and the token scan. `lb_run_stats` gains one field, `uint64_t
setup_ns` — self-timed by the candidate, `LB_STAT_UNSET` when unreported —
and `LB_ABI_VERSION` bumps 2 → 3 (all modules statically linked and rebuilt
together; the runner surfaces the field in the per-query `prefilter` block
with origin `"candidate"`). **No change to the OnPair library is needed**:
its automata do all precompute eagerly in constructors that the glue already
calls as separate statements from `scan()`, so two clock reads in the glue
(~40 ns against 0.7–25 ms queries) measure the split exactly. Stats remain
self-reported diagnostics, never mixed into headline latency (§10).

### The figures this space is designed to produce

1. Latency vs measured selectivity (log-log), per candidate × strategy, per
   op, at fixed length — the crossover points (compressed-domain vs direct).
2. Latency vs needle length at fixed selectivity band — automaton build cost
   vs verify cost.
3. **The heatmap**: selectivity × length grid colored by
   `onpair-compressed / uncompressed-direct` latency ratio — "where OnPair
   is lacking" as one figure.
4. `contains_any` scaling in k, balanced vs skewed.
5. Ratio-vs-latency Pareto per op at selectivity slices, across candidate
   configs (the two-axis headline, §1).
Replicate spread (R needles per cell) renders as error bands; per-query
latency distributions (§9) render as tail comparisons.

### Implementation shape and tests

One new harness module (`gen.rs`) plus a `gen` subcommand; reuses the
dataset loader, oracle, and suite writer; no ABI or results-format change.
Tests: (a) determinism — same seed twice → byte-identical suite on the
fixture dataset; (b) band acceptance unit tests including the 0-band
mutation loop; (c) coverage report accounts for every grid point exactly
once; (d) generated suites pass `bench bless` + `bench check`; (e) a
mini end-to-end: gen (quick) → bless → run on fixtures, gate green.

### Scope decisions (review round 4, 2026-07-08)

(See §15 for the dataset-reproducibility layer that implements decision 2's
dataset axis.)

1. **Micro-sweep first.** The real-workload (macro) track — query sets from
   published benchmarks (ClickBench LIKE queries, TPC-H Q13/Q16, TPC-DS)
   run as workload mixes with source citations in `meta` — is deferred; the
   suite format already accommodates it with no changes.
2. **Dataset axis is the step after the generator.** Columns spanning the
   statistics that drive both axes: URLs (done), natural language
   (e.g. ClickBench `Title`), query/machine logs, and a low-redundancy
   identifier column as the learned-dictionary adversary; each dataset ships
   a statistics table (length distribution, byte entropy, substring
   redundancy) so results are interpretable. Data-size scaling (1M → 10M →
   100M) rides on this step.
3. **Baselines (FSST, LZ4/ZSTD, dictionary) are a later phase.**
   **Filter-only scope locked:** the measured unit is predicate → bitmap;
   materialization of matching rows is out of scope for the paper (argued
   there, revisitable later as an additional strategy phase).
   Case-insensitive matching: future work, acknowledged as hard.
4. **Phase instrumentation approved** — `setup_ns`, ABI v3 (subsection
   above); no OnPair library modification required.
5. **Grid defaults adopted as specced** (absent objections in review):
   per-op grids with R = 5/3, `contains_any` k ≤ 64, dense selectivity
   points {0.3, 0.5, 0.8}, `measure.min_millis` stays 200 (the per-spec
   override already exists for sweep runs that want to halve wall time).

---

## 15. Dataset reproducibility (2026-07-08)

Implemented alongside §14's scope decision 2, modeled on (and tightened
from) the OnPair compression paper's benchmark repo
(github.com/gargiulofrancesco/compression_benchmark):

- **`datasets/sources.yaml`** — the pinned manifest: per entry a source URL,
  raw sha256 (pinned where upstream is immutable, recorded-at-prepare
  otherwise), license id, extraction rule, and the **canonical checksum**
  (`bench ingest`'s logical xxh3) that names the benchmark input itself.
  The default roster is the compression paper's six headline columns
  (TPC-H p_name + c_comment at SF=10, Amazon Books titles, DBpedia
  short abstracts, MS MARCO URLs + queries) plus ClickBench URLs — the two
  papers evaluate the same row streams, extracted by the same rules
  (original file order, same field/strip/skip logic).
- **`datasets/prepare.py`** — one idempotent driver: download (retry +
  sha256) → stdlib extraction → intermediate parquet → `bench ingest` (the
  only artifact producer) → canonical-checksum verify; `--update-checksums`
  pins first-fetch values back into the manifest. A checksum mismatch is a
  hard stop, never a warning.
- **`reproduce.sh`** — staged (`build` / `datasets` / `suites` / `run`),
  with wall-clock estimates in the header. The `suites` stage runs
  `bench gen --seed 42` + `bless` per materialised dataset, so the full
  chain — bytes → dataset checksum → suite → truth → results manifest — is
  a pure function of the manifest plus one seed.

Improvements over the paper repo's scheme: dblp is pinned to a **dated
release** instead of the rolling snapshot; queries are *generated*
deterministically rather than shipped as a static needle file; and the
canonical checksum layer means reviewers verify benchmark-input identity,
not just transport integrity. Divergence: no held-out split — OnPair trains
on the chunk it compresses (build() *is* the training), so a train/eval
split has no meaning on the query axis.

## 16. Uncompressed SOTA: prefilters, evaluators, and the strategy cross-product

*Status: approved (2026-07-08; dictionary encoding cut from this phase); implementing.*

The uncompressed side must be the strongest defensible opponent, and its
internal structure is itself a paper question: **when does SIMD
prefiltering help, and when does it collapse** — especially short vs long
rows, rare vs common needle bytes. Today's scanners are monolithic
(`memmem` silently fuses a rare-pair SIMD prefilter with Two-Way verify),
so that ablation is currently impossible.

### 16.1 What already composes, and what doesn't

For every (candidate, scanner) pair the runner emits up to three strategy
rows — `compressed` (candidate `run()`), `direct`, `decode`. So
decompress-then-eval × evaluator is already automatic: LZ4/zstd need only
decode-only candidates and every scanner composes with them for free
("OnPair decode + teddy+strstr" is just a spec line). What's missing:

1. **Prefilter × evaluator decomposition** — scanners are single
   `prepare`/`scan` units; no way to express "teddy prefilter + strstr
   verify" or measure a prefilter in isolation.
2. **Haystack-level scanning** — all scanners loop row-by-row; on short
   rows (msmarco-query, ~36 B) the SIMD kernel never gets a runway.
   Scanning the concatenated payload once and mapping hit positions back
   to rows is a distinct strategy and the shape compressed-domain scanning
   already has; omitting it handicaps the uncompressed side.
3. **Spec `strategies` allowlist** — the runner runs every applicable
   strategy per pair; a 30-scanner shootout must not re-run `decode` for
   every codec × scanner.

### 16.2 Baseline roster

**Platform policy:** implement every module and *gate* it (cfg(target_arch)
at registration + the existing `cpu_features` hard-gate) rather than omit
it — runs happen on both macOS/arm64 and x86-64, and a module that can't
run on a platform is absent from that platform's results, never silently
scalar.

Single-pattern evaluators (contains/prefix/suffix):

| scanner | notes |
|---|---|
| `memmem` | in already; memchr crate, fused SIMD prefilter + Two-Way, NEON/AVX2 |
| `cpp-std-find` | in already; libc++/libstdc++ `string_view::find` |
| `strstr` | platform libc — glibc's SSE2/AVX2 path on Linux/x86, Apple's on macOS |
| `stringzilla` | modern SIMD library, NEON/AVX-512; competitive with memmem |
| `vectorscan` | Hyperscan fork (identical x86 codepaths, NEON on ARM); single-literal "noodle" engine; feature-gated heavy dep |
| `bndm` | bit-parallel, needles ≤ 64 B — matches the needle-length axis |
| `kmp`, `bmh` | classic reference points (KMP automaton already exists) |

Multi-pattern (contains_any / multi_contains): k× memmem loop (current),
`aho-corasick` (crate; NFA/DFA — BurntSushi's canonical Rust
implementation), `teddy` (`aho_corasick::packed`, explicit — the canonical
port of Hyperscan's), `vectorscan` multi-literal (FDR/Teddy original).

**`ac-cpp` evaluated and dropped (2026-07-09).** The author's hand-written
full-DFA byte-level Aho-Corasick (dense 256-wide goto table) was wrapped as
a scanner and benchmarked head-to-head against `teddy` and the
`aho-corasick` crate on ClickBench URLs and MS MARCO queries. Findings: with
no prefilter it touches every byte, so on single-needle `contains` it ran
~8× slower than the memchr-prefiltered scanners (83 vs ~10 ms on URLs) and
it lost at small k; its one edge is **cost flat in k** (one table lookup per
byte regardless of pattern count), which let it beat `teddy` only at very
high pattern counts (k=64 short rows: 28.8 vs 229 ms). Since it only roughly
matches — and does not clearly beat — the public, maintained `aho-corasick`
crate in that large-k corner, it was **removed as a contender** rather than
maintained as a bespoke C++ dependency. Source is parked (unbuilt) under
`scanners/ac_cpp/` for reference; the workspace `exclude`s it.

Prefilters (composable): `none`, `first-byte`, `rare-byte` (memchr's
static frequency heuristic), `first+last` (bytes at offsets 0, len−1,
movemask AND), `rare-pair` (same mechanism, rarity-chosen offsets —
isolates the heuristic's value vs the fixed choice), `teddy-1`
(single-pattern fingerprint). On ARM the movemask idiom is NEON `shrn`;
memchr/aho-corasick provide both ISAs from the same code.

Two composition granularities, both expressible:
- **position-level**: prefilter emits candidate positions; verify =
  needle memcmp at the position (the high-performance shape, what memmem
  does internally);
- **row-level**: prefilter screens whole rows; survivors go to any named
  evaluator (the clean cartesian, e.g. `teddy-1+strstr`).

A `composed` scanner crate registers each combination as an ordinary named
scanner — no ABI change; composed scanners self-report prefilter
counters through the existing `lb_run_stats` fields, finally exercising
§10's instrumentation on the uncompressed side. Haystack-level variants
are registered as scanner names with a `-hay` suffix (cross-row false
positives resolved by position→row-bounds verification).

Decompress-then-eval candidates: `lz4` (block), `zstd` (at a few levels).
`fsst` is the closest string-native rival — see §17, which also adds the
compressed-domain FSST-LIKE matcher.

### 16.3 Experiment plan: two tiers, combinations stay first-class

A pure tournament ("keep the winning prefilter and evaluator") is unsound:
there is no single winner — we already measured the memmem/Aho-Corasick
crossover at k, and prefilter value swings with needle rarity, row length,
and selectivity. A full cartesian headline (~30 scanner configs × 1,320
targets × 7 datasets × strategies) is unaffordable. So:

- **Tier 1 — screening shootout** (one experiment, one figure): all
  prefilter×evaluator combos + fused engines, `direct` strategy only, on
  three representative columns (proposed: msmarco-query = short rows,
  clickbench-url-1m = URLs, dbpedia-abstract = long text). Output: a
  **winner map** over (op, needle length, selectivity band) — the defense
  of every baseline choice, and a survey figure in its own right.
- **Tier 2 — headline sweeps**: the shootout's per-regime winners +
  `memmem`-as-shipped (the recognizable baseline) + `vectorscan`
  (industrial SOTA), × codecs (decode), × compressed evals, full grid,
  all seven datasets.

Every combination remains spec-addressable forever; tiering is spec
authoring, not architecture.

### 16.4 Work items (post-approval order)

1. Spec `strategies` allowlist (small; unblocks affordable shootouts).
   *Done (2026-07-08).*
2. `composed` scanner crate: prefilter × verifier registry, prefilter stats
   self-reporting. *Done:* `pf-none|first-byte|rare-byte|first-last|rare-pair`,
   row-granularity, position-level exact verify, reporting
   `prefilter_candidates` (survivors) via a timing-mode-free monomorphised
   loop. Row-level "any named evaluator" cartesian (e.g. `teddy-1+strstr`)
   and per-phase `prefilter_ns`/`verify_ns` splits deferred (need per-row
   clocks / cross-crate verifier composition).
3. Haystack-scan variant for contains. *Done:* `memmem-hay`.
4. New scanner crates: `libc-memmem`, `bndm`, `kmp`, `bmh`,
   `aho-corasick`/`teddy` multi, `stringzilla` (crate FFI) — *done.*
   `vectorscan` (FFI, feature-gated) remains: it is untestable on the arm64
   dev host (needs a system libhs/libvectorscan), so it lands during an x86
   run rather than blind here.
5. New candidates: `lz4`, `zstd`. *Done* (decode-only; offsets stored
   uncompressed and counted in footprint).
6. Tier-1 shootout spec (`specs/shootout/tier1.toml`, done) + run;
   winner-map analysis; then Tier-2 specs.

ABI change: this phase bumped the contract to **v4** (optional
`lb_scanner.supports_query` — a pure per-query capability probe so a scanner
declares an out-of-envelope query `Unsupported`, not `Error`; used by `bndm`
for >64 B needles and `teddy` for empty needles).

Open questions for review: vectorscan linkage (system/brew library vs
vendored build); zstd level set; shootout column triple confirmation;
which x86 machine joins the paper's hardware matrix.

## 17. FSST: standard decode baseline and compressed-domain LIKE (FSST-LIKE)

*Status: approved (2026-07-09); implementing.*

FSST (Fast Static Symbol Table, Boncz/Neumann/Leis, VLDB'20) is the
string-native rival OnPair is measured against, and DaMoN'26 published
**FSST-LIKE-Matching** (`calin2110/FSST-LIKE-Matching`, MIT): LIKE/substring
predicates evaluated **directly on FSST-compressed bytes** via a per-pattern
finite automaton, with interpreted, C++-codegen, and LLVM-JIT backends. This
section adds both to the roster so the paper can contrast *decompress-then-scan*
against *match-in-place* within the FSST family, next to the OnPair results.

### 17.1 Two candidates, two forks — by design

- **`fsst`** — standard FSST, **decode-only** (no `run`/`view`), exactly the
  lz4/zstd shape. Wraps **cwida/fsst upstream** (the canonical reference
  implementation). It is the reference-standard decompress-then-eval line: the
  harness composes every scanner over its `decode()` output.
- **`fsst_like`** — the DaMoN compressed-domain matcher. Wraps
  **`calin2110/FSST-LIKE-Matching`**, which transitively fetches
  **`calin2110/fsst`** (a fork of cwida/fsst). It exposes compressed-domain
  `run()` strategies (below) **and** `decode()` (it owns an FSST decoder).

The two deliberately link **different FSST forks**, so their compressed
bytes / ratios are **not byte-identical** (different symbol-table trainers),
though each is internally consistent and both report identical footprint
components. The truly apples-to-apples "decode vs match-in-place on *identical*
bytes" comparison therefore lives **inside `fsst_like`** — its own `decode`
strategy versus its `run` strategies — while `fsst` (cwida) is the
canonical-reference decode line. This split is intentional and is stated here so
the ratio delta between the two rows is read as a trainer difference, not a bug.

Both C++ dependencies are `FetchContent`-pinned to explicit commits (like
onpair), for reproducibility.

### 17.2 Footprint components

Both candidates report the same three components, mirroring lz4/zstd/onpair so
the compression-axis report's `payload×` formula (raw payload ÷ (footprint −
index)) works unchanged:

| component | meaning |
|---|---|
| `payload_fsst` | Σ compressed row byte-lengths (the payload-analog) |
| `symbol_table` | serialized symbol table (`fsst_export`, ≤ ~2 KB) |
| `offsets` | compressed-row index, (rows+1)×8 B, **uncompressed** — the honest index cost, excluded from `payload×` |

### 17.3 `fsst_like` backends, ops, and gating

The compressed-domain matcher is a per-pattern automaton run over each row's
compressed bytes. It ships as one candidate declaring, per host, only the
**viable** strategies:

| strategy | backend | gate |
|---|---|---|
| `interp` | interpreted automaton (`LikePatternAutomatonParser`) | always available; portable |
| `cpp` | C++ codegen → compile `.so` → `dlopen` (no SIMD) | runtime C++ compiler present |
| `cpp-simd` | as `cpp`, SSE `_mm_cmpestrm`/`_mm_cmpeq_epi8` | + `__x86_64__` |
| `llvm` | LLVM ORC-JIT of the automaton (no SIMD) | built with LLVM 16 (`HAVE_LLVM`) |
| `llvm-simd` | as `llvm`, SSE intrinsics | + `__x86_64__` |

Gating follows §16.2's platform policy — **implement every backend, gate the
ones that can't run, never scalar-substitute and never omit.** Unavailable
backends are simply *not declared* in the strategy list (so cells are absent,
not errored). The candidate's whole-module `cpu_features` stays NULL (the
interpreted path is portable); SIMD gating is per-strategy, not whole-candidate.

**Supported ops** per strategy: `PREFIX | SUFFIX | CONTAINS | MULTI_CONTAINS`,
via the automaton types start / end / middle / full. Translation from the ABI
query, escaping `%` `_` `\` inside needles:

| op | LIKE pattern | automaton |
|---|---|---|
| PREFIX `n` | `n%` | start |
| SUFFIX `n` | `%n` | end |
| CONTAINS `n` | `%n%` | middle |
| MULTI_CONTAINS `[a,b,…]` | `%a%b%…%` | full |

**CONTAINS_ANY is Unsupported** — an OR of literals is not expressible as one
LIKE pattern, and the automata are conjunctive (start/middle/end/full) only.
This is a documented capability gap recorded distinctly from an error; a
follow-up may OR N automata.

**Known parser limitation (trailing backslash).** A needle ending in a literal
backslash escapes to `…\\` and, wrapped, yields e.g. `%…\\%`; the FSST-LIKE
parser's end-detection (`pattern[size-2]=='\\'`) mis-reads the closing `%` as
escaped and matches such rows wrong. This is a property of the upstream matcher,
not our wiring; it is rare in real string columns (verified absent in the
current suites) and the correctness gate is the backstop (a tripping query
fails the gate rather than silently reporting a wrong number). All other cases —
including literal `%` and `_` anywhere in the needle — are handled correctly
(validated against a substring oracle).

### 17.4 Per-query cost accounting

For codegen backends the per-query automaton build + (source-gen+compile, or
JIT) is the paper's *preprocess/compile* cost and dominates a single query
while amortizing over many rows. `run()` self-reports it as `setup_ns` (ABI v3).
Headline latency is the harness-clock over the whole `run()` (includes setup);
`setup_ns` is reported alongside so **match-only = headline − setup**. This
reproduces the create/compile/match split the FSST-LIKE benchmarks report.

### 17.5 Correctness and determinism

The compressed-domain matcher must produce results **byte-for-byte identical**
to the canonical uncompressed scan: the standard count + bitmap-hash correctness
gate is its acceptance test and must pass on every suite query before any number
is recorded (else exit 3). `fsst_create` symbol-table training is deterministic
for fixed input, so ratios are stable across runs.

### 17.6 Work items (status)

1. `fsst` (cwida) decode-only candidate + registration + `codecs.toml` entry.
   **Done (2026-07-09):** payload× ≈ 2.0 on msmarco, decode ~2.8 GB/s, gate passes.
2. `fsst_like` interpreted `run()` + `decode()`; **correctness gate passes on all
   four columns.** **Done (2026-07-09).** Finding (`fsst_like_query.toml`):
   compressed-domain `interp` beats decode-then-eval on short/medium rows
   (msmarco 13.4 vs 19.6 ms; url 22.6 vs 26.5; tpch 26.8 vs 31.7) but **loses on
   long rows** (dbpedia 126 vs 85 ms) — the interpreted automaton's per-symbol
   overhead scales with row length, which is exactly the regime the codegen
   backends are built to win. So the interpreted number under-represents
   FSST-LIKE on long text.
3. `cpp` / `llvm` / `-simd` codegen backends — **deferred to an x86 host with
   LLVM 16** (untestable on the arm64 dev box: LLVM 16 absent, SSE `_mm_cmpestrm`
   is x86-only, and `cpp` codegen shells out to `clang++` per query). Same
   deferral rationale as §16.4's vectorscan. Full handoff for the
   x86 agent: **`TODO_fsst_like.md`** at the repo root (architecture, exact
   FSST-LIKE codegen API, per-strategy gating, CMake/LLVM wiring, gotchas,
   validation steps).

Dependencies added (FetchContent-pinned): cwida/fsst, calin2110/FSST-LIKE-Matching
(+ its calin2110/fsst, fmt). LLVM 16 is an *optional* build dependency (`llvm@16`
brew / `llvm-16-dev` apt); absent ⇒ `llvm*` strategies absent, not a build
failure. `vectorscan`/`hybrid_string_search` from the FSST-LIKE repo are **not**
built — those are its own decode baselines, which our `fsst` candidate + the
harness `decode` composition already cover.
