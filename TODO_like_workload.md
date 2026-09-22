# TODO: LIKE workload expansion — `_` wildcards, ClickBench/IMDb/TPC-H columns, adapted TUM corpus

**Status:** Phases A–G implemented on `feat/like-workload-expansion` (rebased onto main after
its suffix-array generator landed; gen2 is built on it). A committed; B–G pending commit.
PR 2 (§15) not started.
**Audience:** the agent (or human) who implements this. Read this file, then
`DESIGN.md` §5 (suite format), §8 (gating), §14 (generator), §15 (dataset
reproducibility), §17 (FSST family), and `contract/SEMANTICS.md`, before
touching code.

---

## 1. Objective

The paper's performance story is currently measured on one query generator
(`gen1`), five literal ops, and seven columns — of which four are URL-ish or
comment-ish text. Nothing in the corpus exercises SQL `LIKE` shapes that the
five ops cannot express, and nothing exercises the `_` single-character
wildcard at all. That is an overfitting risk for every compressed-domain
matcher we are tuning, and it is the one axis the DaMoN'26 TUM paper
("Compression-Aware LIKE: Matching Patterns in the FSST Domain", Pop, Riedl,
Neumann) stresses hardest.

This work:

1. teaches the benchmark to represent and evaluate **arbitrary SQL LIKE
   patterns** (`%`, `_`, `\` escape) with the same correctness-gate rigour as
   the existing ops;
2. adds **seven new columns** across ClickBench, IMDb and TPC-H, chosen from
   measured distributions rather than by name;
3. imports the **upstream TUM pattern corpus** (pinned commit) as an *adapted*
   workload, keeping it distinguishable from our generated queries;
4. adds a **`gen2` generator** that mines patterns from a held-out row split and
   stratifies them by pattern class × literal length × measured selectivity;
5. teaches **Benchmark Explorer 3000™** (`tools/bench-viz`) to plot the result —
   throughput against selectivity, one series per strategy, faceted by pattern
   class, with declined cells shown as coverage rather than silently dropped;
6. keeps every existing blessed suite, result and spec **byte-identical**.

**Two PRs.** This one (`feat/like-workload-expansion`) builds the corpus, the
truth, the capability gating, the plaintext baseline and the figures — no
matcher is modified, so every compressed-domain engine reports `Unsupported` on
the wildcard classes and the plot *measures the gap*. PR 2
(`feat/like-wildcard-execution`, §15) closes it engine by engine, starting with
`fsst_like_tum` and generalizing to the prefilter family and OnPair.

Success is: a candidate cannot produce a timing for a pattern it does not
actually support, and no number in the paper comes from a corpus that only
contains the shapes our matchers happen to be good at.

### It closes a deferral already on the books

`DESIGN.md` §14 "Scope decisions (review round 4)" item 1 deferred the
*macro* track — "query sets from published benchmarks (ClickBench LIKE
queries, TPC-H Q13/Q16, TPC-DS) run as workload mixes with source citations
in `meta`" — noting "the suite format already accommodates it with no
changes". That claim is 80% right: `%`-only shapes need no format change; `_`
and anchored gaps do. This work un-defers item 1 and pays the remaining 20%.

---

## 2. Assumptions (correct these before implementation starts)

1. **Matching stays byte-oriented.** `contract/SEMANTICS.md` is explicit: rows
   are byte strings, no UTF-8 requirement, no case folding. Therefore `_`
   matches **exactly one byte**, not one Unicode codepoint. This differs from
   PostgreSQL/DuckDB `LIKE` on multibyte text and must be documented in
   SEMANTICS.md, not discovered later. The engine under test (upstream
   FSST-LIKE) is byte-oriented too, so baseline and candidate agree.
2. **Backslash is already the repo's escape character** — we are surfacing a
   convention, not inventing one. `candidates/fsst_like_tum/cpp/fsst_like_tum_candidate.cpp:121-127`
   emits `\%`, `\_`, `\\` when it lowers a literal needle into a LIKE pattern,
   and upstream's `src/pattern.cpp` honours it. We adopt exactly that: `\%`
   literal percent, `\_` literal underscore, `\\` literal backslash, a trailing
   lone `\` is a **load error** (upstream mis-parses it; the candidate already
   has `kErrTrailingBackslash` for this).
3. **We do not rewrite any matcher.** `onpair` and the prefilter family will be
   `Unsupported` on `_` patterns. That is the honest outcome and the prompt
   sanctions it.
4. **`multi_contains` really is `%a%b%`.** SEMANTICS.md and
   `harness/src/oracle.rs:56-66` enforce *ordered, non-overlapping, greedy
   leftmost*, and the doc argues greedy-leftmost ≡ SQL for these patterns. We
   rely on that equivalence — and prove it with a test rather than trusting the
   comment.
5. **Timing budget.** `bench bless` runs the naive oracle over the full column
   once per query. gen2 additionally runs exact probes during generation
   (gen1 spent 1 594 exact probes on ClickBench-URL). Expect suite generation
   in the tens of minutes per 1M-row column, not seconds.
6. **No commits or pushes** happen without an explicit instruction (per the
   repo's standing rule).

---

## 3. What exists today, and what is missing

| Concern | Today | Gap this work closes |
|---|---|---|
| Ops | 5 literal ops, closed enum `LB_PREFIX…LB_CONTAINS_ANY` (`abi/src/lib.rs:20-25`) | No anchored-gap (`%a%b`), no `_` |
| Pattern escape | Only inside `fsst_like_tum`, one direction (needles → pattern) | No suite-level pattern representation |
| Oracle | Literal matchers + differential twin + 4 000-case randomized test (`harness/src/oracle.rs`) | No LIKE evaluator |
| Capability gating | Op mask on every strategy; **per-query probe exists for scanners only** (`LbScanner.supports_query`, ABI v4). `Strat::Candidate` returns `true` unconditionally (`harness/src/runner.rs:63-72`) | A candidate cannot decline one specific pattern |
| Skip accounting | `Status::Ok \| Status::Unsupported => summary.cells_ok += 1` (`harness/src/runner.rs:506`) | A skip is counted as a pass in the run summary |
| Generator | `gen1`: op × selectivity band × length × k/f × mix, needles sampled from the **full** column (`harness/src/gen.rs:479-505`) | No held-out mining split, no pattern-shape axis, no ultra-rare bucket |
| Selectivity strata | Already good: bands `0, 1e-5, 1e-4, 1e-3, 1e-2, 1e-1, 0.3, 0.5, 0.8` (`gen.rs:148-161`), recorded in `meta.gen.band`, measured value in `derived.selectivity` | Missing "1..10 rows" bucket; no bucket column in the results DB |
| Length bands | `L1,2,4,8,16,32,64` (+128 for prefix) | Add 12 (SIMD boundary); define "length" for patterns |
| Datasets | 7 pinned (`datasets/sources.yaml`), 4 non-default extras | ClickBench Referer/Title, IMDb, TPC-H p_type/s_comment/o_comment |
| TUM corpus | Engine vendored and pinned; **`benchmark/patterns.json` is already in the fetched tree and unused** | Not imported as a suite |
| Results DB | `query` table keyed on op + needle + selectivity (`analysis/db/schema.sql:120-139`) | No pattern text, wildcard counts, or bucket columns |
| Fixtures | `datasets/mini` (200 rows) + `suites/smoke` (28 queries) + gate canary | No wildcard-adversarial fixture |

### Candidate capability map (measured, not assumed)

Ops: **P**refix **S**uffix **C**ontains **M**ulti **A**ny.

| module | strategies | ops | LIKE-pattern path? |
|---|---|---|---|
| `fsst_like_tum` | `interp`, `cpp`, `cpp-simd`, `llvm`, `llvm-simd` | P S C M | **yes** — `to_like_pattern`, and upstream has `UnderscorePattern` |
| `dict_fsst_like_tum` | `dict+interp` | P S C M | **yes** — same lowering |
| `fsst_like_utn` | `comet` | C M | partial: `%`-joins needles, **no** `_` (its algos explicitly reject `_`) |
| `uncompressed_memmem` | `memmem`, `memmem-hay` | all / C | no |
| `onpair` | `compressed` | P C A | no |
| `onpair_spiral` | `pf_kmp`, `pf_memmem`, `kmp` | C | no |
| `dict_onpair`, `dict_onpair_spiral` | `dict+…` | C | no |
| `uncompressed_prefilter`, `dict_*_prefilter`, `fsst_prefilter`, `fsst_decode_prefilter`, `llm_token_prefilter` (+ `dict_` peers) | one each | C A | no |
| `fsst`, `lz4`, `zstd`, `llm_token_tum`, `cpp_identity`, `uncompressed` | — (decode/view only) | — | n/a — they inherit whatever scanner the harness composes |

**The leverage point:** the decode-only codecs have no ops of their own; they
are measured through the harness-composed `decode` strategy plus a scanner. So
**one new LIKE scanner gives LIKE numbers to `fsst`, `lz4`, `zstd`,
`llm_token_tum`, `cpp_identity` and the uncompressed baseline at once**, with
zero per-candidate work.

---

## 4. Design decisions (locked)

**D1 — One new op, `like`, one needle = the raw pattern.**
`{"op":"like","needles":["%ab_c%"]}`. `DESIGN.md:334-337` already sanctions
exactly this: *"`op` is an open string in the format with a closed, versioned
enum in the ABI. A future general pattern op (`'a%b_c'`) … can be added without
breaking existing suites."* ABI: `LB_LIKE = 5`, `LB_OP_COUNT = 6`,
`LB_ABI_VERSION` 7 → 8. Arity: exactly 1.

**D2 — Generated and imported patterns are stored under the narrowest op that
provably means the same thing.** Lowering table, applied once at
generation/import time:

| pattern shape | stored op | example |
|---|---|---|
| `lit%` | `prefix` | `MEDIUM POLISHED%` |
| `%lit` | `suffix` | `%BRASS` |
| `%lit%` | `contains` | `%BRASS%` |
| `%a%b%…%` (leading **and** trailing `%`, ≥2 literals) | `multi_contains` | `%special%requests%` |
| anything else — anchored gaps, any `_` | `like` | `%Customer%Complaints`, `Customer%Account%`, `%BR_SS%` |

Every query — lowered or not — carries `meta.like.pattern` (the canonical
pattern text), so analysis groups by LIKE shape regardless of stored op. The
whole existing roster therefore keeps competing on the `%`-only majority of the
corpus, with no candidate changes; only genuinely new shapes narrow to
LIKE-capable modules. **The equivalence is a test, not a comment**
(§7, T-EQ).

**D3 — The oracle gains `like_matches`, in the oracle's own style.** Naive,
allocation-free, no memchr, no shared machinery with any candidate — plus an
independent differential twin (a DP-table implementation) in the test module,
matching the existing `twin_matches` pattern, and the existing randomized
differential harness extended to LIKE patterns.

**D4 — Capability declaration becomes per-query for candidates too.** Add
`LbCandidate.supports_query(this, strategy_index, query) -> i32`, optional
(NULL ⇒ the op mask is the whole answer). This mirrors ABI v4's proven
`LbScanner.supports_query` rather than inventing a new mechanism, and is what
lets `fsst_like_tum` accept `%a_b%` while declining a pattern its parser
rejects. `runner.rs` already calls `strat.supports_query(...)` and maps a
`false` to `Status::Unsupported` — only the `Strat::Candidate` arm needs to
stop returning a hardcoded `true`.

**D5 — A skip is not a pass.** Split `RunSummary::cells_ok` into `cells_ok` /
`cells_unsupported`, print both, and keep `status` in `results.jsonl` as-is
(it already distinguishes them; only the summary conflates them).

**D6 — New scanner `like` (and `like-hay`) over plaintext.** A real evaluator
(literal-run scanning with `memchr` between wildcards, backtracking for `%`,
byte-skip for `_`), declaring `LB_LIKE` plus the four literal ops it subsumes.
This is the uncompressed baseline *and*, through `decode`, the baseline for
every codec. It is **not** the oracle and shares no code with it.

**D7 — No matcher learns `_` in this PR.** Wildcard *execution* is PR 2 (§15):
every compressed-domain candidate reports `Unsupported` on `op = like` here,
which is the honest state and the baseline PR 2 improves on. The per-query probe
from D4 still gets a real user in this PR — `gate_canary` grows two strategies:
one that declares `LB_LIKE` and declines underscore patterns through
`supports_query`, and one that declares `LB_LIKE` and silently lowers `%ab_c%`
to `contains("abc")`, which the gate must catch. The ABI bump ships here so PR 2
needs no second one.

**D8 — A new fixture dataset, not an edit to `datasets/mini`.** Editing
mini changes its canonical checksum, which invalidates all 28 blessed truths in
`suites/smoke` and the gate-canary spec that shares them — directly against
acceptance criteria 1 and 9. Instead: `datasets/fixtures/wildcards.csv` →
`datasets/wildcards` + `suites/wildcards` + `specs/wildcards.toml`. Same
adversarial rows the prompt asks for, zero blast radius. *(Deviation from the
prompt's letter; recorded here deliberately.)*

**D9 — TUM corpus is "adapted", never "reproduced".** Our physical columns
differ from the paper's (modern IMDb vs their 2017 frozen dump; DuckDB dbgen
vs theirs; we skip their StackOverflow and PublicBI datasets entirely). Suite
descriptions say *adapted TUM LIKE workload*, and every query keeps
`meta.tum.{repo, commit, dataset, table, column, underscore_group, tpch_query}`.

**D10 — `gen2` is a new generator version beside `gen1`, never a mutation of
it.** `gen1` output stays reproducible byte-for-byte; `bench gen --generator
gen2` is a new flag. Existing suites are never regenerated.

**D11 — Figures come from Benchmark Explorer 3000™, not a new plotting stack.**
`tools/bench-viz/bench_viz.py` already builds the exact figure this workload
needs: x = **Selectivity**, y = **Throughput (GB/s)**, one toggleable series per
candidate/strategy/scanner, median lines with IQR bands, log axes, focus range
(`tools/bench-viz/template.html:53-69`). It needs four things to understand this
corpus, and nothing else (Phase G):

1. it drops every row whose `status != "ok"` (`bench_viz.py:117`), so an
   `Unsupported` series silently disappears instead of being reported as a
   coverage gap — the plotting counterpart of D5;
2. `SUBSTRING_OPS` (`bench_viz.py:109`) and the Operation selector do not know
   `like`;
3. with D2 lowering, one LIKE corpus spreads across five ops, so *Operation* is
   no longer the right facet — **Pattern class** and **underscore count** have to
   sit beside it;
4. the aggregate control bins x into 8/12/16 equal bins; the suite's own
   selectivity buckets are the honest grouping, so it gains a *Suite bucket*
   aggregation mode.

Everything else — series toggles, PNG export, the detail panel, the Prefilter
tab — works unchanged.

---

## 5. Commands

```bash
# build + test (the gate for every task below)
cargo build --release
cargo test --release

# fixture-level verification (fast, no big datasets)
cargo test --release -p lb-harness oracle::          # LIKE semantics units
cargo test --release --test like_semantics           # new integration test
target/release/bench run specs/wildcards.toml -o /tmp/wild   # adversarial fixture
target/release/bench run specs/smoke.toml     -o /tmp/smoke  # must stay green
target/release/bench run specs/gate-canary.toml -o /tmp/can  # must exit nonzero

# datasets (new entries only; existing ones are already materialised)
.venv/bin/python datasets/prepare.py --dataset clickbench-referer-1m \
    --dataset clickbench-title-1m --dataset tpch-ptype-sf10 \
    --dataset tpch-scomment-sf10 --dataset tpch-ocomment-sf1 \
    --dataset imdb-primary-title-1m --dataset imdb-primary-name-1m

# suites
target/release/bench gen --method like --seed 42 \
    --dataset datasets/clickbench-referer-1m --out suites/clickbench-referer-1m-gen2-s42
target/release/bench bless --suite suites/clickbench-referer-1m-gen2-s42 \
    --dataset datasets/clickbench-referer-1m
target/release/bench tum-import --patterns <pinned patterns.json> \
    --dataset datasets/tpch-ptype-sf10 --table part --column p_type \
    --out suites/tum_like/tpch-ptype-sf10
target/release/bench check --suite <suite> --dataset <dataset>   # truth reproduces

# runs
target/release/bench run specs/like-quick.toml          -o results/like-quick
target/release/bench run specs/paper/like-cross-dataset.toml -o results/paper/like-cross-dataset
target/release/bench run specs/paper/tum-like-adapted.toml   -o results/paper/tum-like-adapted

# figures — Benchmark Explorer 3000™ (throughput vs selectivity, one series per strategy)
python3 tools/bench-viz/bench_viz.py results/paper/like-cross-dataset \
    --queries suites --title "LIKE throughput vs selectivity" \
    --out tools/bench-viz/out/like-cross-dataset.html
python3 tools/bench-viz/bench_viz.py results/paper/tum-like-adapted \
    --queries suites/tum_like --title "Adapted TUM LIKE workload" \
    --out tools/bench-viz/out/tum-like-adapted.html
python3 -m pytest tools/bench-viz/tests          # explorer statistics + normalization
```

---

## 6. Project structure (files this work touches)

```
contract/lb_candidate.h              MOD  LB_LIKE, LB_OP_COUNT=6, ABI v8,
                                          lb_candidate.supports_query
contract/SEMANTICS.md                MOD  LIKE section: %, _, \ escape, BYTE semantics,
                                          edge cases, lowering-equivalence claim
abi/src/lib.rs                       MOD  mirror of the above, op_name/op_from_name
harness/src/oracle.rs                MOD  like_matches + twin + fixtures + randomized diff
harness/src/suite.rs                 MOD  arity for `like`; pattern validation (escape,
                                          trailing backslash); derived.* additions
harness/src/runner.rs                MOD  Strat::Candidate per-query probe; cells_unsupported
harness/src/registry.rs              MOD  ABI v8 validation
harness/src/gen/like.rs              NEW  `--method like`: pattern classes, held-out mining
                                          pool on main's SubstringIndex, exact probing, lowering
harness/src/tum_import.rs            NEW  patterns.json -> suite (+ provenance)
harness/src/main.rs                  MOD  `--method like` (main's flag convention), `tum-import`
harness/tests/like_semantics.rs      NEW  T-LIKE-*, T-EQ, T-CAP (see §7)
harness/tests/gen2.rs                NEW  determinism, bucket coverage, truth protection
scanners/like/                       NEW  `like` + `like-hay` LIKE evaluator scanner
candidates/fsst_like_tum/cpp/…       MOD  declare LB_LIKE, passthrough, supports_query
candidates/dict_fsst_like_tum/cpp/…  MOD  same
datasets/fixtures/wildcards.csv      NEW  adversarial rows (tracked; see .gitignore rule)
datasets/sources.yaml                MOD  7 new default + 3 new non-default entries
datasets/prepare.py                  MOD  imdb-tsv-field + imdb-list-block extractors
datasets/README.md                   NEW  why these columns, measured profiles, licences
suites/wildcards/                    NEW  fixture suite (blessed)
suites/<dataset>-gen2-s42/           NEW  generated suites
suites/tum_like/<dataset>/           NEW  adapted TUM suites
specs/wildcards.toml                 NEW  fixture spec
specs/like-quick.toml                NEW  ~40-query dev spec
specs/paper/like-cross-dataset.toml  NEW  ClickBench + IMDb + TPC-H, onpair vs baselines
specs/paper/tum-like-adapted.toml    NEW  TUM compatibility/correctness spec
analysis/db/schema.sql               MOD  query.pattern, pattern_class, pct_wildcards,
                                          underscore_count, selectivity_bucket, length_bucket
analysis/db/load.py                  MOD  populate them
analysis/report.py                   MOD  group by selectivity bucket, never grand mean alone
tools/bench-viz/bench_viz.py         MOD  `like` op; carry pattern/class/bucket fields;
                                          keep Unsupported cells as coverage
tools/bench-viz/template.html        MOD  Pattern class + Underscore selectors,
                                          "Suite bucket" aggregation option
tools/bench-viz/app.js               MOD  filter/aggregate/report on the above
tools/bench-viz/tests/               MOD  pytest + jsc coverage for the new fields
tools/bench-viz/figures.sh           NEW  the paper figures, one command
DESIGN.md                            MOD  new §18 (this design, folded in on completion)
reproduce.sh                         MOD  gen2 stage beside the gen1 stage
```

Untouched, deliberately: `datasets/mini`, `suites/smoke`, `suites/fsst_like_guard`,
`suites/*-gen1-s42`, `suites/compression/*`, `specs/paper/clickbench-*`,
`specs/compression/*`, `specs/shootout/*`, `results/**`.

---

## 7. Testing strategy

pytest is not in play here — this is `cargo test --release`, fixtures in the
oracle's existing style (deliberately naive, seeded, no external rand crate).

| id | test | asserts | maps to acceptance criterion |
|---|---|---|---|
| T-LIKE-1 | `oracle::like_*` fixtures | `%ab_c%` matches `zzabXczz`, `zzab-czz`, `zzab_czz`; does **not** match `zzabczz`, `zzabXYczz` | 3, 4 |
| T-LIKE-2 | escape fixtures | `%ab\_c%` matches only a literal `_`; `\%` literal; `\\` literal; trailing lone `\` is a load error | 3 |
| T-LIKE-3 | anchoring fixtures | `a%`, `%a`, `%a%`, `a%b`, `%a%b`, `a%b%`, empty pattern, `%`, `_` on empty row | 3 |
| T-LIKE-4 | differential twin | naive backtracker vs DP twin over 4 000 seeded random (pattern, row) pairs from a tiny alphabet incl. `%`, `_`, `\`, `\x00`, `\xff` | 3 |
| T-EQ | lowering equivalence | for every lowering rule in D2 and 4 000 seeded rows: `row_matches(lowered_op, needles, row) == like_matches(pattern, row)` | 3 |
| T-FIX | `specs/wildcards.toml` end-to-end | blessed truth on the adversarial fixture is exactly the hand-computed match sets; run exits 0 | 4 |
| T-CAP | capability gating | a candidate without `_` support emits `Status::Unsupported`, **no latency**, and increments `cells_unsupported` not `cells_ok`; a silently-lowering stub is caught by the gate | 5 |
| T-GEN2-1 | determinism | same seed ⇒ byte-identical `queries.jsonl` | 6 |
| T-GEN2-2 | bucket accounting | every grid point is filled / partial / empty-with-reason in `gen-report.json`; populated buckets recorded per dataset | 6 |
| T-GEN2-3 | truth protection | regeneration refuses to clobber a blessed suite without `--force`; bless verifies rather than overwrites | 7 |
| T-TUM | import | every imported pattern round-trips its provenance; underscore-mutated patterns get **freshly computed** truth (assert a mutated pattern's count ≠ its parent's, where they differ) | 7 |
| T-REG-1 | smoke unchanged | `git diff --exit-code datasets/mini suites/smoke` is clean; `specs/smoke.toml` run exits 0 | 1, 9 |
| T-REG-2 | canary still fires | `specs/gate-canary.toml` exits nonzero with a gate failure | 2 |
| T-REG-3 | no large files | `git status --porcelain` shows no `datasets/*/data.arrow`, `*.parquet`, `results/**` | 8 |
| T-VIZ-1 | `tools/bench-viz/tests/test_bench_viz.py` | a `like` row normalizes with its pattern, class, wildcard counts and bucket; an `unsupported` row becomes coverage, not a data point, and never a silent drop | — |
| T-VIZ-2 | `tools/bench-viz/tests/render.test.js` | "Suite bucket" aggregation reproduces the per-bucket medians computed independently in Python, to four digits (the existing two-implementations-agree discipline) | — |

Coverage expectation: every new public function in `oracle`, `gen2` and
`tum_import` has at least one fixture test; the LIKE evaluator scanner is
covered indirectly by T-FIX and directly by the gate on every suite it runs.

---

## 8. Boundaries

**Always**
- Run `cargo test --release` before declaring a task done.
- Compute truth with the oracle only; a candidate never produces its own truth.
- Keep `meta` opaque to the harness; put anything the harness computes in
  `derived`.
- Record source URL + commit/sha256 for anything imported or downloaded.

**Ask first**
- Any change to an existing blessed suite, `datasets/mini`, or anything under
  `results/`.
- Any ABI change beyond the two additions in D1/D4.
- Adding a dataset that is not in §9's table.
- Making `_` execution work inside `onpair` (explicitly out of scope).

**Never**
- Lower a `_` pattern to `contains` / `multi_contains` / a stripped literal.
- Report a timing for a cell whose gate did not pass.
- Count an `Unsupported` cell as a correctness pass.
- Commit materialised datasets or results (`.gitignore` already covers this —
  verify, do not weaken).
- `git commit` or `git push` without an explicit instruction.

---

## 9. Datasets to add

Measured on this machine today (ClickBench from the already-local
`hits_0.parquet`; TPC-H from the already-local SF10 DuckDB), non-empty rows
only:

| column | rows | non-empty | avg B | p50 | p95 | max | U/N | MB |
|---|---|---|---|---|---|---|---|---|
| ClickBench `URL` *(have)* | 1 000 000 | 100.0% | 88.6 | 74 | 195 | 1991 | 0.276 | 88.6 |
| ClickBench `Referer` | 1 000 000 | 92.1% | 86.4 | 68 | 195 | 2007 | 0.247 | 79.6 |
| ClickBench `Title` | 1 000 000 | 93.6% | 147.9 | 121 | 424 | 1026 | **0.079** | 138.4 |
| ClickBench `SearchPhrase` | 1 000 000 | **6.9%** | 50.9 | 45 | 109 | 1939 | 0.264 | 3.5 |
| ClickBench `OriginalURL` | 1 000 000 | 14.9% | 186.5 | 143 | 443 | 3723 | 0.606 | 27.8 |
| ClickBench `Params` | 1 000 000 | 0.0% | — | — | — | — | — | 0.0 |
| ClickBench `MobilePhoneModel` | 1 000 000 | 2.0% | 4.2 | 4 | 6 | 17 | 0.002 | 0.1 |
| TPC-H `part.p_type` | 2 000 000 | 100% | 20.6 | 21 | 24 | 25 | **0.0001** | 41.2 |
| TPC-H `supplier.s_comment` | **100 000** | 100% | 62.4 | 62 | 97 | 100 | 0.996 | 6.2 |
| TPC-H `orders.o_comment` SF10 | **15 000 000** | 100% | 48.5 | 48 | 75 | 78 | 0.528 | **727.4** |
| TPC-H `part.p_name` *(have)* | 2 000 000 | 100% | 32.7 | 33 | 39 | 51 | 0.9999 | 65.5 |
| TPC-H `customer.c_comment` *(have)* | 1 500 000 | 100% | 72.5 | 72 | 112 | 116 | 0.935 | 108.7 |

Reading: `Referer` is URL-like with a different host mix; `Title` is the only
long natural-language column with *heavy duplication* (U/N 0.079 — a dictionary
front-end's best case and FSST's, quite unlike URL); `p_type` has ~150 distinct
values in 2M rows, the extreme low-cardinality LIKE target the TUM paper leans
on. `SearchPhrase` contradicts the prompt's guess: only 69 k of 1M rows are
non-empty, so it becomes a **non-default extra under an honest id**.
`Params`, `MobilePhoneModel`, `OriginalURL` are rejected — empty, degenerate,
or a near-duplicate of `URL`.

### New `sources.yaml` entries

| id | source | default? | notes |
|---|---|---|---|
| `clickbench-referer-1m` | `hits_0.parquet` (already pinned, already local), column `Referer` | yes | zero download |
| `clickbench-title-1m` | same file, column `Title` | yes | zero download |
| `clickbench-searchphrase-69k` | same file, column `SearchPhrase`, non-empty only | **no** | honest id: 69 k rows |
| `tpch-ptype-sf10` | `duckdb-extension:tpch` sf=10 `part.p_type` | yes | SF10 DB already generated |
| `tpch-scomment-sf10` | same DB, `supplier.s_comment` | yes | small (100 k rows) — flagged in the dataset README |
| `tpch-ocomment-sf1` | `duckdb-extension:tpch` sf=**1**, `orders.o_comment` | yes | 1.5 M rows, ~73 MB; one cheap local dbgen |
| `tpch-ocomment-sf10` | sf=10, `orders.o_comment` | **no** | 15 M rows / 727 MB |
| `imdb-primary-title-1m` | `https://datasets.imdbws.com/title.basics.tsv.gz` (227 MB) | yes | first 1 M non-empty `primaryTitle` in file order, `\N` dropped |
| `imdb-primary-name-1m` | `https://datasets.imdbws.com/name.basics.tsv.gz` (310 MB) | yes | first 1 M non-empty `primaryName` |
| `imdb-plot-frozen-1m` | `https://ftp.fu-berlin.de/pub/misc/movies/database/frozendata/plot.list.gz` (160 MB) | yes | TUM's actual plot corpus |
| `imdb-quotes-frozen-1m` | `.../frozendata/quotes.list.gz` (87 MB) | yes | TUM's actual quotes corpus |

**IMDb reproducibility — the load-bearing finding.** `datasets.imdbws.com`
files are **regenerated daily** (`title.basics.tsv.gz` last-modified today), so
a permanent `sha256` pin is impossible there. Those two entries therefore use
the repo's existing `recorded-at-prepare` convention *plus* a recorded
`snapshot_date`, and pin the **canonical xxh3 of the extracted column** — which
is what actually binds suites to data. A new snapshot then fails loudly at
suite load (`suite.rs:190-205` rejects a checksum mismatch) instead of silently
producing incomparable numbers.

By contrast `ftp.fu-berlin.de/.../frozendata/` is genuinely frozen
(`plot.list.gz` last-modified **2017-12-22**) and is fully sha256-pinnable —
which is why the TUM-reproduction columns come from there. Both IMDb sources
are non-commercial-research-only; `datasets/README.md` states the terms
(https://www.imdb.com/interfaces/) rather than pretending otherwise.

Extraction needs two new `prepare.py` kinds, both streaming (never unpack a
full `.list` to disk):
- `imdb-tsv-field`: gzip TSV with a header line, field by **name**, drop `\N`
  and empties, `limit` rows.
- `imdb-list-block`: the 2017 `plot.list` / `quotes.list` block format
  (`PL:` continuation lines / quote blocks) → one string per record, `limit`
  rows.

---

## 10. Suites

### Generated (`gen2`, seed 42)

`suites/<dataset>-gen2-s42/` for each of: `clickbench-url-1m`,
`clickbench-referer-1m`, `clickbench-title-1m`, `msmarco-query`,
`dbpedia-abstract`, `tpch-pname-sf10`, `tpch-ptype-sf10`,
`tpch-ccomment-sf10`, `tpch-scomment-sf10`, `tpch-ocomment-sf1`,
`imdb-primary-title-1m`, `imdb-primary-name-1m`.

`gen2` grid axes:

- **pattern class** — `prefix`, `suffix`, `contains`, `multi_gap` (`%a%b%`),
  `anchored_gap_head` (`a%b`), `anchored_gap_tail` (`%a%b`), `underscore_1`,
  `underscore_2plus`, `mixed` (`%speci_l%requ_sts%`). Target mix follows the
  TUM paper's shape — ~20% prefix, ~20% suffix, ~60% unanchored — with the
  wildcard classes layered on top.
- **literal length** — total non-metacharacter bytes:
  `1, 2, 4, 8, 12, 16, 32, 64, 128` (128 where the column's rows permit).
  Preserves the existing `L…` band naming.
- **selectivity band** — the nine existing bands plus **`ultra_rare`**, a
  *count*-based band accepting 1..10 matching rows (a new `BandKind::Count`;
  the existing bands are ratio-based and 1e-5 already ≈ 10 rows at 1M, so
  ultra-rare needs its own kind to be meaningful on larger columns).
- **replicates** — as gen1 (5 for single-literal classes, 3 otherwise).

`gen2` additions over `gen1`:

- **Held-out mining pool.** Candidate literals are drawn only from rows where
  `xxh3(row_index) % 8 == 0` — a stable, deterministic ~12.5% split, recorded
  as `meta.gen.mining_pool = "xxh3_mod8_eq0"`. Truth and every timed scan still
  use the **full** column. This keeps pattern discovery off the rows a
  compressor's symbol table is most likely trained on.
- **Underscore mutation.** A `_` replaces a **single ASCII byte** of an
  accepted literal (never a byte inside a multi-byte UTF-8 sequence — the
  pattern would still be well-defined, but the mutation would be
  uninterpretable), and the mutated pattern is **re-probed from scratch**. A
  mutated pattern never inherits its parent's count.
- **`derived` additions** (harness-computed, trusted):
  `pattern`, `pattern_class`, `literal_len_total`, `percent_count`,
  `underscore_count`, `selectivity_bucket`, `length_bucket`.

### Adapted TUM (`suites/tum_like/`)

Source: `benchmark/patterns.json` from `calin2110/FSST-LIKE-Matching` — already
present in the fetched tree at
`target/release/build/lb-cand-fsst-like-*/out/build/_deps/fsst_like_src-src/benchmark/patterns.json`.
**Pin the upstream commit explicitly** (the CMake currently pins our fork
`Hedi-Chehaidar/FSST-LIKE-Matching@09d89812`, which is upstream `b1eb3ab9` +
two kernel fixes; `patterns.json` is untouched by those). Record repo + full
sha in `suite.json.provenance` and copy the pinned file into the repo under
`suites/tum_like/patterns.json` so imports do not depend on a build tree.

Upstream inventory (counted):

| upstream dataset | `_`=0 | `_`=1 | `_`≥2 | total | we import? |
|---|---|---|---|---|---|
| TPCH (`p_type`, `p_name`, `o_comment`, `s_comment`) | 23 | 27 | 28 | **78** | **yes** — all four columns are in our roster |
| IMDB (`films`, `actors`, `plot`, `quotes`) | 80 | 80 | 79 | **239** | **yes** — `films`→`imdb-primary-title-1m`, `actors`→`imdb-primary-name-1m`, `plot`/`quotes`→frozen columns |
| StackOverflow (`Posts`, `Comments`, `PostHistory`) | 16 | 15 | 18 | 49 | no — dataset not in our roster (documented gap) |
| PublicBI (6 columns) | 96 | — | — | 96 | no — same |
| **total** | | | | **462** | **317 imported** |

Per-query metadata: `meta.tum = {repo, commit, upstream_dataset, table, column,
underscore_group, tpch_query}` (`tpch_query` is upstream's `query` field —
Q2/Q9/Q13/Q14/Q16/Q20 — null where absent). Truth is computed fresh by the
oracle against **our** column; TUM match counts are never imported.

Expect many TUM patterns to land at selectivity 0 on our physical columns
(e.g. IMDB patterns mined from 2017 plot text against modern `primaryTitle`).
That is information, not failure: the import records achieved selectivity per
pattern and `datasets/README.md` reports how many patterns survived into a
non-zero bucket per column. Zero-hit patterns stay in the suite — the zero
bucket is a legitimate stratum.

### Fixture

`suites/wildcards` over `datasets/wildcards` (rows: `abxc`, `ab-c`, `ab_c`,
`abc`, `abXYc`, `xxabxczz`, `prefix-middle-suffix`,
`prefixXXmiddleYYsuffix`, plus empty row, single byte, `\x00`/`\xff` rows,
a row containing a literal `%`, and a row containing a literal `\`).
Patterns chosen so that each pair distinguishes exactly one semantic axis:
`%ab_c%` vs `%abc%` vs `%ab\_c%`; `a%b` vs `%a%b%` vs `%a%b`; `_` vs `%` on the
empty row.

---

## 11. Specs

| spec | contents | scale |
|---|---|---|
| `specs/wildcards.toml` | fixture dataset + `uncompressed_memmem`, `like` scanner, `fsst_like_tum`, `gate_canary` | seconds |
| `specs/like-quick.toml` | ~40 representative queries (one per pattern class × bucket) on one ClickBench and one TPC-H column | minutes — the dev loop |
| `specs/paper/like-cross-dataset.toml` | gen2 suites for ClickBench (url, referer, title) + IMDb (title, name) + TPC-H (pname, ptype, ccomment, scomment); candidates: `uncompressed_memmem`, `uncompressed_prefilter`, `onpair`, `onpair_spiral`, `fsst`, `fsst_like_tum` (interp), `lz4`, `zstd`, dict peers; scanners `memmem`, `like` | hours — the headline |
| `specs/paper/tum-like-adapted.toml` | all `suites/tum_like/*`; candidates: `fsst_like_tum` (all backends **except** `cpp*`/`llvm*` per the existing per-query-compile caveat in `clickbench-url-1m-contains.toml`), `dict_fsst_like_tum`, `fsst`+`like` scanner, `uncompressed_memmem`, `uncompressed`+`like` | hours |

Every spec keeps its `strategies` allowlist **above the first `[[table]]`
header** — the repo has already been bitten by TOML binding a root key to the
last table (`spec.rs:5-29`; an 83-minute surprise). Giant optional datasets
(`tpch-ocomment-sf10`, `clickbench-searchphrase-69k`) appear in no default spec.

The same spec files are reused verbatim by PR 2 — that is the point. In this PR
`fsst_like_tum` answers the TUM patterns that lower to `prefix` / `suffix` /
`contains` / `multi_contains` (roughly the `_`=0 group) and declines the rest;
in PR 2 the declined cells fill in, against an already-published baseline.

---

## 12. Implementation plan

Six phases. Each ends green (`cargo test --release`) and leaves the tree
working; each is one atomic conventional commit (`feat:` / `test:` / `docs:`),
made **only when instructed**.

### Phase A — LIKE semantics core *(no datasets, no candidates)*
- [x] A1 `contract/SEMANTICS.md`: LIKE section — `%`, `_`, `\` escape, **byte**
      semantics, edge cases (empty pattern, `_` on empty row, `%` only,
      trailing lone `\` invalid), and the D2 lowering-equivalence claim.
      *Verify:* review. *Files:* 1.
- [x] A2 `contract/lb_candidate.h` + `abi/src/lib.rs`: `LB_LIKE`,
      `LB_OP_COUNT=6`, `LB_ABI_VERSION=8`, `op_name`/`op_from_name`.
      *Verify:* `cargo build --release`. *Files:* 2.
- [x] A3 **RED then GREEN**: `oracle::like_matches` + twin + fixtures
      (T-LIKE-1..4) + T-EQ. *Verify:* `cargo test --release -p lb-harness`.
      *Files:* 1.
- [x] A4 `suite.rs`: arity + pattern validation for `like`; reject trailing
      lone `\`; `derived` additions. *Verify:* unit tests. *Files:* 1.
- [x] A5 `datasets/fixtures/wildcards.csv` + `suites/wildcards` +
      `specs/wildcards.toml`, blessed. *Verify:* T-FIX. *Files:* 4.

### Phase B — capability plumbing + the LIKE baseline
- [x] B1 ABI v8 `LbCandidate.supports_query`; `registry.rs` validation;
      `runner.rs` `Strat::Candidate` arm. *Verify:* T-CAP. *Files:* 4.
- [x] B2 `cells_unsupported` split in `RunSummary` + CLI summary line.
      *Verify:* T-CAP. *Files:* 2.
- [x] B3 `scanners/like` (`like`, `like-hay`). *Verify:* gate passes on
      `specs/wildcards.toml`. *Files:* 3 + workspace `Cargo.toml`.
- [x] B4 `gate_canary`: a `like-declines` strategy (declares `LB_LIKE`, returns
      0 from `supports_query` for `_`) and a `like-lowers` strategy (declares
      `LB_LIKE`, evaluates `%ab_c%` as `contains("abc")`). The first must be
      counted `Unsupported`, the second must fail the gate. *Verify:* T-CAP.
      *Files:* 1.
- [x] B5 Regression gate: T-REG-1/2/3. *Verify:* smoke + canary + `git status`.

### Phase C — datasets
- [x] C1 `sources.yaml`: the 11 entries in §9 (7 default, 4 non-default).
- [x] C2 `prepare.py`: `imdb-tsv-field`, `imdb-list-block`; non-empty filter for
      the parquet path where the entry asks for it.
- [x] C3 Materialise the zero-download ones (ClickBench ×2, TPC-H ×3) and pin
      canonical checksums. *Verify:* `prepare.py` re-run is a no-op.
- [x] C4 Materialise IMDb (4 columns); record snapshot dates + sha256.
- [x] C5 `datasets/README.md`: measured profiles (§9 table), why each column was
      chosen and each rejected, IMDb non-commercial terms, TPC-H dbgen pinning
      (DuckDB `tpch` extension version + SF).

### Phase D — adapted TUM corpus
- [x] D1 Copy the pinned `patterns.json` into `suites/tum_like/`; record repo +
      full commit sha.
- [x] D2 `bench tum-import`: parse → lower per D2 → emit one suite per
      (dataset, column) with full `meta.tum`. *Verify:* T-TUM.
- [x] D3 Import TPC-H (78) + IMDb (239); bless; record achieved selectivity and
      how many patterns land in each bucket per column.
- [x] D4 Document the StackOverflow/PublicBI gap and every difference from an
      exact reproduction.

### Phase E — `gen2`
- [x] E1 `gen2.rs`: pattern-class grid, held-out mining pool, `BandKind::Count`
      ultra-rare band, underscore mutation with fresh probing, D2 lowering.
- [x] E2 `--generator` CLI flag; `gen1` path untouched. *Verify:* gen1 suites
      regenerate byte-identically (T-GEN2-1 applied to gen1 too).
- [x] E3 `harness/tests/gen2.rs` (T-GEN2-1..3).
- [x] E4 Generate + bless the 12 gen2 suites; `reproduce.sh` gains a gen2 stage.

### Phase F — specs, analysis, docs
- [x] F1 The four specs in §11.
- [x] F2 `analysis/db/schema.sql` + `load.py`: pattern/class/wildcard/bucket
      columns; `analysis/report.py` groups by selectivity bucket and never
      reports a grand mean alone.
- [x] F3 `DESIGN.md` §18: fold this design in; update §14's scope-decision-1
      note to "done".
- [ ] F4 Final report (§13) — in the PR description.

### Phase G — Benchmark Explorer 3000™ (the figures)
- [x] G1 `bench_viz.py`: add `like` to `SUBSTRING_OPS` (:109) and every other
      op-enumerating site; carry `pattern`, `pattern_class`, `percent_count`,
      `underscore_count`, `selectivity_bucket`, `length_bucket` into the
      embedded point record (:156-205); show the pattern text in the detail
      panel beside Needle (:906). *Verify:* T-VIZ-1.
- [x] G2 Stop dropping non-`ok` rows at :117. Keep `unsupported` cells as a
      per-series coverage count — the series legend reads
      `fsst_prefilter — 0/142 supported` instead of the series vanishing.
      Gate-failed and errored cells stay excluded from the plot but are counted
      too. *Verify:* T-VIZ-1.
- [x] G3 Template + `app.js`: **Pattern class** and **Underscore count**
      selectors beside Operation; a *Suite bucket* option in the Aggregate
      control that groups by `selectivity_bucket` instead of equal-width bins.
      *Verify:* T-VIZ-2.
- [x] G4 `tools/bench-viz/figures.sh`: regenerate the paper figures from
      `results/paper/like-cross-dataset` and `results/paper/tum-like-adapted`
      in one command; document them in `tools/bench-viz/README.md`.
- [ ] G5 (needs a real run) The headline figure: throughput (GB/s) against selectivity, one line
      per strategy, faceted by pattern class, with the coverage line stating
      which strategies declined which classes. Exported PNG + the HTML explorer
      checked into neither (both are `results/`-shaped artifacts) but
      reproducible by G4.

**Dependency order:** A → B → {C, D, E can overlap once A+B land; D needs C for
the IMDb/TPC-H columns; E needs C} → F → G (G needs a real run to plot, but G1/G2
can be written against `results/paper/clickbench-url-1m-contains`, which already
exists on disk).

---

## 13. Deliverable at the end

A report containing: files changed; new dataset ids + public sources; new suite
names + query counts; distribution by dataset × pattern class × selectivity
bucket × length bucket × underscore count; the candidate-support matrix per
query class; tests and commands run with their results; datasets not
materialised and why; every difference between our adapted TUM workload and an
exact reproduction; and a handful of example generated patterns with their
measured match counts and selectivities.

---

## 14. Open questions

1. **`bench tum-import` as a subcommand vs a Python script under `datasets/`.**
   Proposed: a Rust subcommand, so pattern parsing and the D2 lowering share
   exactly one implementation with `gen2` and the oracle. A Python importer
   would duplicate the lowering rules in a second language — the classic place
   for a semantic drift bug.
2. **Whether `clickbench-title-1m` should be capped.** At 138 MB / 1M rows with
   U/N 0.079 it is the most interesting new column *and* the slowest to bless.
   A `-500k` variant would halve suite-generation wall time. Proposed: keep 1M,
   measure, revisit if bless is painful.
3. **`tpch-scomment-sf10` is only 100 k rows.** It is the authentic TUM column,
   but small enough that per-query timing noise will dominate. Proposed: keep
   it, flag it in the README, and let the cross-dataset spec carry the weight.
4. **Does any TUM IMDb pattern group land entirely at selectivity 0 on our
   modern columns?** Unknown until D3 runs. If `plot`/`quotes` patterns are all
   zero against `primaryTitle`, the frozen columns become load-bearing rather
   than optional.

---

## 15. PR 2 — wildcard execution (`feat/like-wildcard-execution`)

Separate branch, separate PR, starts from this one. PR 1 builds the corpus,
the truth, the gate and the plaintext baseline, and **measures how much of it
each compressed-domain engine has to decline**. PR 2 closes that gap engine by
engine. Splitting it this way means PR 1 is reviewable without any matcher
change, and PR 2 has a published "before" number to beat on day one.

The correctness story does not change: every newly-supported cell still passes
the same gate against the same oracle truth, and the `like` scanner from D6
remains the reference the compressed paths are compared against.

### W1 — `fsst_like_tum`, `dict_fsst_like_tum` *(cheapest; do it first)*

Upstream already has full `_` support — `struct UnderscorePattern`
(`include/pattern.hpp`) and the leading/inner/trailing underscore counting in
`src/pattern.cpp:102-145`, reached through the same automaton the `interp`,
`cpp`, `cpp-simd`, `llvm`, `llvm-simd` backends already use. The candidate
*deliberately escapes* `_` today
(`fsst_like_tum_candidate.cpp:121-127`), so the change is: for `op == LB_LIKE`,
pass the pattern through verbatim instead of escaping it, add `LB_LIKE` to
`kLikeOps` (`:450`), and return 0 from `supports_query` for the shapes upstream
rejects (trailing lone backslash — `kErrTrailingBackslash` already exists).
Roughly 20 lines plus tests. `dict_fsst_like_tum` is the same edit in its own
file; `DictMatcher::supported_ops` delegates to its child (`dict_matcher.hpp:99`),
so the dictionary peer inherits the capability once the child has it.

`fsst_like_utn` stays unsupported and is documented as such: its `Comet`,
`Memmem`, `StdFind`, `Skipping` and `StartsWith` dispatchers explicitly test
`pattern.find('_') == npos` to *reject* underscores, and its `MetaStateMachine`
splits on `%` only.

### W2 — the prefilter family

`uncompressed_prefilter`, `fsst_prefilter`, `llm_token_prefilter`,
`fsst_decode_prefilter` and their `dict_` peers all share one shape: build a
mandatory code cover for the needle, prune rows that cannot contain it, verify
the survivors exactly. That shape generalizes to wildcards without any new
theory:

1. **Split the pattern into maximal literal runs.** `%speci_l%requ_sts%` has
   runs `speci`, `l`, `requ`, `sts`. Every run is mandatory whatever separates
   them, so the existing per-needle cover builder applies unchanged to each run.
2. **AND the covers.** A row survives only if every run's cover survives. This
   is a *weaker* filter than the literal case and that is fine — it is a filter,
   not an answer.
3. **Verify exactly.** Survivors go through the LIKE evaluator, which must be
   factored out of `scanners/like` into a small shared internal library in W2 so
   there is exactly one implementation of the semantics on the fast path. The
   oracle stays separate and naive, as always.
4. **Decline when the prune is worthless.** `%a_b%` has two one-byte runs; its
   cover will prune nothing and the prefilter would be pure overhead. The
   existing `profitable_hint` machinery (`LbQueryFacts.profitable_hint`, ABI v6)
   already expresses this. Add a run-length floor: below it, `supports_query`
   returns 0 and the cell is honestly `Unsupported` rather than a bad number.

A tighter filter is available later and deliberately deferred: `a_c` is a
*fixed-length window with a don't-care byte*, which a code-level cover can
exploit far better than `a` AND `c`. Ship the correct weak version first, then
measure whether the tighter one is worth it.

### W3 — `onpair`, `onpair_spiral`, `dict_onpair`, `dict_onpair_spiral`

`onpair`'s `compressed` strategy currently declares P | C | A;
`onpair_spiral` declares C across `pf_kmp`, `pf_memmem`, `kmp`. Two routes:

- **W3a — prune then decode-verify (in scope).** Run the existing
  compressed-domain machinery on the *longest* literal run of the pattern, then
  decode only the surviving rows and finish with the shared LIKE evaluator.
  Correct by construction, reuses everything, and gives OnPair a real `_` number
  instead of a blank cell. Its risk is honest and expected: for a pattern whose
  longest run is short, decode-verify can be slower than plain decode-then-LIKE.
  That is a result — and the profitability gate from W2.4 is how it is handled,
  not hidden.
- **W3b — a true compressed-domain LIKE automaton over OnPair tokens (out of
  scope).** This is the actual research contribution and a much larger piece of
  work. Do it only if W3a shows the verify step dominating, and only as its own
  PR.

### W4 — measurement and figures

Rerun `specs/paper/like-cross-dataset.toml` and
`specs/paper/tum-like-adapted.toml` unchanged, and rebuild the explorers with
`tools/bench-viz/figures.sh`. The coverage line added in G2 turns from
`fsst_prefilter — 0/142 supported` into a real ratio, and the PR 2 figure is the
one the paper actually wants: compressed-domain `_` execution against
decode-then-LIKE, throughput vs selectivity, per pattern class.

### PR 2 acceptance criteria

1. Every newly-supported cell passes the existing gate — no exceptions, no
   relaxed comparison.
2. No pattern is transformed on the way in: `%ab_c%` is never answered by
   `contains("abc")`, `contains("ab") && contains("c")`, or a stripped literal.
   The `like-lowers` gate-canary strategy from B4 stays in the suite as the
   standing proof that such a transformation is caught.
3. A candidate that declines a pattern class does so through `supports_query`
   and is counted `Unsupported`, never as a pass.
4. The plaintext `like` scanner remains the correctness reference and the
   published baseline.
5. The PR 1 → PR 2 coverage delta is reported per candidate × pattern class.
