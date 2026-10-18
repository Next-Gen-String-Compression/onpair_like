# Design: a two-axis benchmark for LIKE-family string predicates

Status: reference spec for the harness, the candidate contract and the
measurement rules the paper's numbers rest on. Code comments cite it by
section, so the numbers stay fixed: §12, §13 and §15 (review notes and the old
dataset roster) were removed, and §3, §11, §16 and §17 keep only what still
describes the code.

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
  matrix loudly instead of the whole run. This gives the isolation of separate
  processes without putting IPC anywhere near the timing path — the child
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

### Selection vectors (ABI v7)

A LIKE predicate rarely runs first; a cheaper predicate upstream has usually
already admitted a fraction of the rows. The spec's `[[selections]]` list
puts that on the axis: `all`, `uniform` (each row admitted independently
with `fraction`), or `runs` (one run per period of `avg_run / fraction`
rows, its length drawn uniformly over `run_rows_min..=run_rows_max`), each
seeded and decided from the global row index so the admitted rows are the
same at every `chunk_rows`. The harness hands a strategy that declares
`LB_STRATEGY_TAKES_SELECTION` each chunk's ascending row list through
`lb_query.selection`; every other strategy runs over all rows and the
harness ANDs its result with the selection inside the timed sample. Truth
under a selection is the oracle ANDed with it, computed at run time, so no
suite is re-blessed. Result rows carry `selection` and `selection_rows`.

The harness-composed `decode` strategy takes the selection when every chunk
offers `decode_selected` (ABI v8): it decodes the selected rows only, packed,
the scanner runs over them, and each hit is moved back to its row.

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

**Writing a spec — root keys go above the first `[[table]]` header.**
`strategies` and `measure` belong to the document root, and TOML assigns a
bare key-value pair to the most recently opened table, so a `strategies`
line written *after* the `[[scanners]]` block becomes a field of the last
scanner. Every spec struct sets `serde(deny_unknown_fields)`, so that
mistake is now a load error naming the key and the fields it could have
been (`harness/src/spec.rs`); before the guard it parsed happily and the run
silently used the unfiltered strategy set.

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
- Derived per-value normalizations stored with each result: `ns_per_value`
  (median over the column's rows), `ns_per_domain_value` (median over the
  values actually *evaluated* — see §10's evaluation domain), and effective
  bytes/s over the *raw* payload — these make cross-dataset comparison
  meaningful regardless of each candidate's compression.
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

### The result store: a derived index, not a second source of truth

`results.jsonl` is the record; `results/bench.duckdb` is a rebuildable index
over it, loaded at the end of every `bench run` (`tools/results-db/`). Five
dimensions — platform, run, dataset, system, query — plus one measurement table
whose rows carry latency *and* the footprint of the build they were answered
from, so the two axes of §9 sit side by side without a join. The store adds no
information: it can be deleted and reproduced from `results/**` at any time, and
it is gitignored for that reason.

Two properties follow from this section rather than from convenience. The DB
never aggregates across platforms — every row carries its `hostname`/`arch` and
the store offers no view that merges them, because cross-machine comparison is
side-by-side and explicit. And it keeps what a rerun destroys: a run overwrites
`results.jsonl` in place, so each ingest is a new `run` row (the newest per
output directory flagged `is_latest`), preserving history the wire format
cannot. Rows a run *withheld* stay visible: `unsupported` cells and the
recorded absences (§9's module gating) are loaded, not filtered.

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
  `lb_run_stats` — minimally `prefilter_candidates` (values surviving its
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

**The evaluation domain (ABI v5).** A candidate does not necessarily
evaluate one value per row. A dictionary front-end answers the predicate
once per *unique* value and scatters the verdict to rows through the codes,
so everything it counts — survivors included — is in that reduced domain.
Such a module declares `eval_domain` (the values it evaluated) and
`eval_domain_matches` (the true matches among them); leaving both UNSET
asserts one evaluation per row. These are structural counters, identical in
every sample, which is why they are the one instrumented-mode quantity the
harness *does* divide into the timing-mode median.

That gives two per-value costs, and their ratio is the dictionary's whole
contribution:

- `ns_per_value` = median / num_rows — the end-to-end cost. Evaluating
  fewer values for the same answer shows up here, as it should.
- `ns_per_domain_value` = median / eval_domain — the cost per value
  actually evaluated, so it compares *engines* rather than pipelines. Equal
  to `ns_per_value` for every row-domain module.
- `ns_per_value = ns_per_domain_value / (num_rows / eval_domain)` — so a
  `dict_X`-vs-`X` speedup factors into the dedup it bought and the change in
  the engine's own per-value cost (scatter overhead, and the cache locality
  of a smaller working set).

**Derived attribution — computed by the harness, not trusted from the
candidate.** From `prefilter_candidates` plus the true match count in the
same domain, the harness derives per query:

- prune rate: `1 − candidates/eval_domain`
- false-positive rate of the prefilter:
  `(candidates − domain_matches) / candidates`
- verify cost per survivor: timing-mode latency divided by candidates
  (an honest approximation; exact splits come from the optional self-timed
  phases, clearly labeled as self-reported)

All three take their denominator from the domain the work happened in.
Scoring a dictionary's unique-domain survivor count against the row count
is what made `false_positive_rate` come out negative for the `dict_*`
candidates before v5.

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

## 11. Repository layout

The layout table in [README.md](README.md) lists every directory. Design
choices that differ from a plain reading of the brief:

- Truth is count + set-hash (with debug samples), not a stored row set;
  stored full sets don't scale with high selectivity.
- The correctness reference lives in the harness as the oracle, not as a
  candidate, so it is independent of candidate machinery.
- Timing runs in-process, with one child process per candidate for
  isolation, which keeps the fidelity of FFI and the isolation of separate
  processes.
- Instrumentation is a separate execution mode, so inspecting the prefilter
  and trusting the latency numbers never trade off.
- The deliberately wrong candidate is the registered `gate_canary`, with an
  `ok` strategy (a correct naive matcher) and a `wrong` one (bit flip on row
  0); `specs/gate-canary.toml` must exit 3.
- The loader reads the IPC file into aligned buffers (one copy at startup)
  rather than mmapping it, which §4 allows; load cost is setup, not
  measurement.
- `decode()` output buffers carry `LB_DECODE_PAD` (64 B) of writable
  headroom past the payload (SEMANTICS.md rule 8), so fixed-stride
  over-copying decoders (OnPair's 16-byte token copy, FSST's per-symbol copy)
  need no private buffer or defensive copy inside the timed decode.

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
   `onpair_spiral/pf_walk / uncompressed/direct` latency ratio — "where OnPair
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

---

## 16. Uncompressed baselines and the strategy cross-product

The uncompressed side has to be the strongest defensible opponent.

### 16.1 What composes

For every (candidate, scanner) pair the runner emits up to three strategy
rows: `compressed` (candidate `run()`), `direct` (the scanner over the raw
view) and `decode` (the scanner over `decode()` output). Decompress-then-search
therefore needs only a decode-only candidate, and every scanner composes with
it. The spec's root `strategies` list picks which of these run.

All row scanners loop row by row, and on short rows a SIMD kernel never gets a
runway. `memmem-hay` scans a chunk's concatenated payload once and maps each
hit back to its row, the shape compressed-domain scanning already has; the
paper's baselines use it.

### 16.2 Baseline roster

**Platform policy:** implement every module and gate it (`cfg(target_arch)` at
registration plus the `cpu_features` hard gate) rather than omit it. A module
that cannot run on a platform is absent from that platform's results, never
silently scalar.

| scanner | notes |
|---|---|
| `memmem`, `memmem-hay` | the memchr crate: fused SIMD prefilter + Two-Way, NEON/AVX2; per row and per chunk |
| `cpp_std_find` | libc++/libstdc++ `string_view::find` |
| `stringzilla` | SIMD library, NEON/AVX-512 |
| `bndm`, `kmp`, `bmh` | classic references (`classics`); `bndm` takes needles up to 64 B |
| `prefilter` | the C++ matcher of `candidates/common/prefilter` over raw rows |

Decompress-then-search candidates: `lz4` (block) and `fsst` (§17).
`uncompressed_memmem` adds memmem per row and over the whole payload with
selection vectors.

ABI v4 added the optional `lb_scanner.supports_query`, a per-query capability
probe, so a scanner declares an out-of-envelope query `Unsupported` rather
than `Error` (`bndm` above 64 B).

## 17. FSST: standard decode baseline and compressed-domain LIKE (FSST-LIKE)

FSST (Fast Static Symbol Table, Boncz/Neumann/Leis, VLDB'20) is the
string-native rival OnPair is measured against, and DaMoN'26 published
**FSST-LIKE-Matching** (`calin2110/FSST-LIKE-Matching`, MIT): LIKE/substring
predicates evaluated **directly on FSST-compressed bytes** via a per-pattern
finite automaton, with interpreted, C++-codegen, and LLVM-JIT backends. This
section adds both to the roster so the paper can contrast *decompress-then-scan*
against *match-in-place* within the FSST family, next to the OnPair results.

### 17.1 Two candidates, two forks — by design

- **`fsst`** — standard FSST, **decode-only** (no `run`/`view`), exactly the
  lz4 shape. Wraps **cwida/fsst upstream** (the canonical reference
  implementation). It is the reference-standard decompress-then-eval line: the
  harness composes every scanner over its `decode()` output.
- **`fsst_like_tum`** — the DaMoN compressed-domain matcher, **match-in-place only**
  (compressed-domain `run()` strategies, **no `decode()`/`view()`**). Wraps
  **`calin2110/FSST-LIKE-Matching`**, which transitively fetches
  **`calin2110/fsst`** (a fork of cwida/fsst). It is deliberately not a codec:
  its whole point is answering LIKE queries *without* decompressing, so the
  decode-then-scan baseline is left entirely to the `fsst` candidate rather than
  duplicated here.

The two deliberately link **different FSST forks**, so their compressed
bytes / ratios are **not byte-identical** (different symbol-table trainers),
though each is internally consistent and both report identical footprint
components. `fsst` (cwida) is the canonical-reference **decode** line;
`fsst_like_tum` (calin) is the **match-in-place** line — the two axes of the FSST
comparison, one candidate each. (An earlier revision also exposed a `decode`
strategy on `fsst_like_tum` for a same-bytes decode-vs-match contrast; it was
dropped because plain `fsst` already covers decode, and the cross-fork ratio
delta below is the only thing that needs stating: read it as a trainer
difference, not a bug.)

Both C++ dependencies are `FetchContent`-pinned to the commits in `pins.env`.

### 17.2 Footprint components

Both candidates report the same three components, mirroring lz4 so
the compression-axis report's `payload×` formula (raw payload ÷ (footprint −
index)) works unchanged:

| component | meaning |
|---|---|
| `payload_fsst` | Σ compressed row byte-lengths (the payload-analog) |
| `symbol_table` | serialized symbol table (`fsst_export`, ≤ ~2 KB) |
| `offsets` | compressed-row index, (rows+1)×8 B, **uncompressed** — the honest index cost, excluded from `payload×` |

### 17.3 `fsst_like_tum` backends, ops, and gating

The compressed-domain matcher is a per-pattern automaton run over each row's
compressed bytes. It ships as one candidate declaring, per host, only the
**viable** strategies:

| strategy | backend | gate |
|---|---|---|
| `interp` | interpreted automaton (`LikePatternAutomatonParser`) | always available; portable |
| `cpp` | C++ codegen → compile `.so` → `dlopen` (no SIMD) | runtime C++ compiler present |
| `cpp-simd` | as `cpp`, SSE `_mm_cmpestrm`/`_mm_cmpeq_epi8` | + `__x86_64__` |
| `llvm` | LLVM ORC-JIT of the automaton (no SIMD) | built with LLVM 14–16 (`HAVE_LLVM`) |
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

### 17.6 Dependencies

FetchContent-pinned: cwida/fsst, calin2110/FSST-LIKE-Matching
(+ its calin2110/fsst, fmt). LLVM **14–16** is an *optional* build dependency
(`llvm-14-dev`/`llvm-16-dev` apt, `llvm@16` brew); absent (or ≥17, which removed
APIs the upstream codegen uses) ⇒ `llvm*` strategies absent, not a build failure.
The pin needs exactly one LLVM-version compat guard (`toPtr<T>()` is 15+-only),
applied as a FetchContent `PATCH_COMMAND` — see
`candidates/fsst_like_tum/cpp/llvm14_compat.cmake`, the sole source deviation
from upstream. `vectorscan`/`hybrid_string_search` from the FSST-LIKE repo are **not**
built — those are its own decode baselines, which our `fsst` candidate + the
harness `decode` composition already cover.
