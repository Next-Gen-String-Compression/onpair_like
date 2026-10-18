# LIKE-benchmark

A two-axis benchmark — **compression × query latency** — for LIKE-family
string predicates (`prefix`, `suffix`, `contains`, `multi_contains`,
`contains_any`) over real-world string columns. A result is only meaningful
as a (compression, latency) pair.

Design: [DESIGN.md](DESIGN.md) (the reference spec). Contract:
[contract/lb_candidate.h](contract/lb_candidate.h) +
[contract/SEMANTICS.md](contract/SEMANTICS.md).

## Quickstart (fixture dataset)

```sh
cargo build --release

# 1. ingest: raw source -> canonical artifact (cached, deterministic)
./target/release/bench ingest --source datasets/fixtures/mini.csv \
    --format csv --column data --id mini --out datasets/mini

# 2. bless: oracle -> cached ground truth, bound to the dataset checksum
./target/release/bench bless --suite suites/smoke --dataset datasets/mini

# 3. run: every selected cell, correctness-gated, two-axis measured
./target/release/bench run specs/smoke.toml -o results/smoke

# the gate must be seen to fire: this run MUST exit 3
./target/release/bench run specs/gate-canary.toml -o results/gate-canary
```

Outputs: `results/<run>/results.jsonl` (one self-contained row per build /
per gated query cell) + `manifest.json` (environment, versions, checksums,
spec hash). Every run also loads itself into `results/bench.duckdb`, the
queryable index over all runs — see [tools/results-db/README.md](tools/results-db/README.md).

## Generating query workloads

The paper's needle suites come from
[experiments/matcher_selection](experiments/matcher_selection/README.md), the
[shared SA + LCP generator](harness/src/gen/README.md) over complete columns.

## Dashboard

```sh
.venv/bin/pip install -r tools/dashboard/requirements.txt
.venv/bin/marimo run tools/dashboard/dashboard.py        # http://localhost:2718
```

Reads `results/bench.duckdb` read-only. Pick one run (newest first), then read
selectivity against GB/s per evaluation: raw per-query points, the median per
selectivity bin as a line, and the 25/75 percentile as a band behind it. Bins
are three per log decade, drawn as vertical gridlines; the x axis toggles
between log and linear. Chips pick which kernels are drawn, and the table under
the chart gives each one's strategy, scanner, version, config, compression ratio
and build time. The machine and run environment sit above the chart.

Whole-column measurements are shown when the run has them, else its smallest
`chunk_rows`; queries that match nothing are absent, since a log axis has no
room for selectivity 0. Stop the app before starting a `bench run` — DuckDB
will not grant the loader a write lock while the dashboard holds the file open.

## Reproducing everything

```sh
./reproduce-neon.sh   [--small|--medium|--large]   # on an arm64 Mac
./reproduce-avx512.sh [--small|--medium|--large]   # on an x86 host with AVX-512
```

Each runs the paper's blocks for its instruction set from a clone, sibling
repos and data included; with both machines' `results/` and `paper/data` in
one tree, `paper/make-figures.sh` draws every figure. `datasets/sources.yaml`
pins every dataset (URL, sha256, license, canonical checksum), the needle
suites are a pure function of the dataset checksum and seed, and every run
records its spec hash and environment.

## Layout

| path | what |
|---|---|
| `contract/` | the C-ABI contract, the cross-language source of truth |
| `abi/` | Rust mirror of the contract (`lb-abi`) |
| `harness/` | Rust harness: CLI (`bench`), formats, loader, oracle, gate, timing, results |
| `candidates/` | storage schemes: `uncompressed` and `uncompressed_memmem` (plain baselines), `lz4` and `fsst` (decode-then-search), `fsst_like_tum` (TUM's FSST-LIKE matching), `onpair_spiral` (OnPair: `pf_walk` in the compressed domain, and decode), `dict_onpair` (`onpair_spiral` over a chunk's unique values), `fsst_prefilter` (the min-cut prefilter in the FSST fork), the `_debug` variants for the ablation, and `gate_canary` and `cpp_identity` for the harness tests |
| `scanners/` | uncompressed search kernels: `memmem` and `memmem-hay` (Rust `memchr`), `cpp_std_find`, `stringzilla`, `classics` (BNDM, KMP, Horspool), `prefilter` |
| `experiments/matcher_selection/` | the paper's needle suite generator |
| `paper/` | specs, gen scripts, pinned sweeps and plot scripts for every figure |
| `e2e/` | DuckDB and Vortex end to end: ClickBench, JOB and the micro needles |
| `suites/`, `specs/` | the smoke suite and specs the harness tests run |
| `datasets/` | `sources.yaml` (pinned URLs, sha256, licenses, canonical checksums) and `prepare.py`; artifacts and downloads are gitignored |
| `tools/` | `results-db/` (builds `results/bench.duckdb` from `results/**` at the end of each run), `dashboard/` (marimo), `bench-viz/` (static HTML explorer), `graph-viz/` (alignment graph and cut drawings) |

## Adding a candidate or scanner

Implement `contract/lb_candidate.h` (C/C++: own directory + CMakeLists +
copy-paste glue crate; Rust: one crate against `lb-abi`), then register it
with one feature-gated line in `harness/src/registry.rs` and one line in
`harness/Cargo.toml`. See `candidates/cpp_identity` and `scanners/memmem`
as the two patterns.
