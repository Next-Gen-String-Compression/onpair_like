# LIKE-benchmark

The benchmark harness and every experiment behind the paper's figures. It measures
LIKE-family string predicates (`prefix`, `suffix`, `contains`, `multi_contains`,
`contains_any`) on compressed string columns, reporting each system as a pair of compression
ratio and query latency. Every query result is checked against a naive reference scan before
it is timed.

The harness and its C ABI are specified in [DESIGN.md](DESIGN.md),
[contract/lb_candidate.h](contract/lb_candidate.h) and
[contract/SEMANTICS.md](contract/SEMANTICS.md).

## Reproducing the paper

The paper measures on two machines, an arm64 Mac (NEON) and an x86 server with AVX-512. Each
has one script that runs everything from a fresh clone: build and tests, the four dataset
columns, the needle suites, every benchmark block for its instruction set, and on NEON the
end-to-end runs.

```sh
bash reproduce-neon.sh   [--small|--medium|--large] [--prefix-suffix] [--min-iters=N]
bash reproduce-avx512.sh [--small|--medium|--large] [--min-iters=N]
```

The anonymized download drops executable bits. The reproduce scripts run every other script
through `sh` or `bash` and do not need them; to run single scripts as `./…`, first run
`find . -name '*.sh' -exec chmod +x {} +`.

| flag | effect |
|---|---|
| `--small`, `--medium`, `--large` | needles per grid cell: 1, 10 or 20 (211, 2,053 or 4,052 needles); default `--small` |
| `--prefix-suffix` | also run the PREFIX/SUFFIX block (NEON only, about 50 min at `--small`) |
| `--clickbench` | also run the end-to-end ClickBench queries over the full table, which no figure in the paper draws (NEON only, 14 GB more downloads, about 90 GB more disk) |
| `--min-iters=N` | timed iterations per cell for every harness run, overriding each spec |

Approximate wall clock:

| script | `--small` | `--medium` |
|---|---|---|
| `reproduce-neon.sh` | 5 to 6.5 h | 10 to 11 h |
| `reproduce-avx512.sh` | 5 to 6 h | 7.5 to 8.5 h |

The size flag changes the micro, phases, ablation, plan-cut and end-to-end micro blocks. The
row group block always uses the small needle set and takes about 3.5 h on AVX-512 by itself.

Runs land in `results/` and kernel sweeps in `paper/data/`. With both machines' results in one
tree, `sh paper/make-figures.sh --out DIR` checks the inputs `paper/config.py` names and draws
every figure. [paper/README.md](paper/README.md) describes each figure block, its specs and its
inputs.

### Requirements

- hardware: for `reproduce-neon.sh` an arm64 Mac (the paper's is an Apple M4 Pro); for
  `reproduce-avx512.sh` an x86-64 Linux host with AVX-512BW, such as the paper's AWS
  `c7a.xlarge` (AMD EPYC 9R14). The script refuses a host without AVX-512BW: the AVX-512
  kernels are compile-time gated, so such a host would measure other kernels under the
  AVX-512 run names.
- Rust 1.91 or newer, a C++20 compiler, CMake, Python 3.11 or newer
- [`uv`](https://docs.astral.sh/uv/) for the plot and check scripts, and the `duckdb` CLI for
  the kernel sweeps and the end-to-end data
- a TeX installation with the `libertine`, `zi4` and `newtxmath` packages, to draw figures
- for the end-to-end block on NEON, `ninja` and `flatc`, the FlatBuffers compiler the OnPair
  Vortex build runs (`brew install ninja flatbuffers`); the Vortex folders pin Rust 1.98,
  which rustup installs on first use
- on x86, optionally LLVM 14 to 16 for the compiled-automaton baseline (`llvm-simd`).
  `reproduce-avx512.sh` uses an installed one or downloads LLVM's 16.0.4 release into
  `target/llvm-16`; without it that baseline is skipped. The release's static libraries also
  need the zstd, libxml2 and ncurses development packages (`libzstd-dev libxml2-dev
  libncurses-dev` on Ubuntu), which the script installs when `sudo` needs no password; it
  warns when the baseline was not built.

Disk: about 15 GB for the four columns and builds. The end-to-end block on NEON adds about
9 GB of JOB and micro-column data under `$E2E_DATA` (default `e2e/data`), and `--clickbench`
about 90 GB more for the full ClickBench table.

## Quick check on a fixture

A few seconds, no downloads:

```sh
cargo build --release
python3 -m venv .venv && .venv/bin/pip install -r datasets/requirements.txt

./target/release/bench ingest --source datasets/fixtures/mini.csv \
    --format csv --column data --id mini --out datasets/mini
./target/release/bench bless --suite suites/smoke --dataset datasets/mini
./target/release/bench run specs/smoke.toml -o results/smoke

# a deliberately wrong candidate: this run has to exit 3
./target/release/bench run specs/gate-canary.toml -o results/gate-canary
```

`ingest` turns a raw column into a checksummed dataset, `bless` computes the ground truth of a
query suite with the reference scan, and `run` measures every candidate, strategy and scanner a
spec selects. A run writes `results/<run>/results.jsonl` (one row per build and per query cell)
and `manifest.json` (environment, versions, checksums), and loads itself into
`results/bench.duckdb`, the database the figures read.

## Layout

| path | contents |
|---|---|
| `contract/`, `abi/` | the C ABI every candidate and scanner implements, and its Rust mirror |
| `harness/` | the `bench` CLI: data formats, reference scan, correctness gate, timing, results |
| `candidates/` | the compared systems (below) |
| `scanners/` | search kernels over uncompressed bytes: `memmem` and `memmem-hay` (Rust `memchr`), `cpp_std_find`, `stringzilla`, `classics` (BNDM, KMP, Horspool), `prefilter` |
| `experiments/matcher_selection/` | the needle suite generator (SA + LCP over complete columns) |
| `datasets/` | pinned sources of the four columns (`sources.yaml`) and `prepare.py`, which downloads and ingests them |
| `paper/` | specs, run scripts, pinned kernel sweeps and plot scripts for every figure |
| `e2e/` | end to end in DuckDB and Vortex: ClickBench, JOB and the micro needles |
| `third_party/` | modified upstream code the paper measures (below) |
| `tools/` | `results-db/` (builds `results/bench.duckdb`), `dashboard/`, `bench-viz/`, `graph-viz/` |
| `specs/`, `suites/` | the smoke and gate-canary specs and the smoke suite the tests use |

Candidates:

| candidate | what it measures |
|---|---|
| `simd_cut_onpair` | OnPair: the SIMD cut scan in the compressed domain (`simd-cut`), and decompress then search |
| `dict_simd_cut_onpair` | `simd_cut_onpair` over a chunk's distinct values |
| `simd_cut_fsst` | the same cut scan over FSST codes |
| `fsst` | FSST, decompress then search |
| `fsst_like_tum` | FSST-LIKE matching (DaMoN'26): interpreted and LLVM-compiled automata on FSST codes |
| `lz4` | LZ4, decompress then search |
| `uncompressed`, `uncompressed_memmem` | plain strings: a scanner over the whole column, or memmem per row and over the payload |
| `simd_cut_onpair_debug`, `simd_cut_fsst_debug` | the ablation switches (sorted symbol table, uniform frequencies) |
| `gate_canary`, `cpp_identity` | harness tests: a wrong candidate the gate must catch, and a C++ identity codec |

## Third-party code

`third_party/` holds the code the paper modifies, as plain folders, so a clone builds without
fetching it:

| folder | contents |
|---|---|
| `onpair/` | OnPair with the SIMD cut scan |
| `fsst/` | FSST with the SIMD cut scan |
| `duckdb/` | DuckDB with the compressed-domain `contains` on FSST, without its `data/` and `test/` |
| `vortex-0.86.1/`, `vortex-onpair/` | Vortex 0.86.1 and Vortex with OnPair strings, each with a per-file segment cache |

The SIMD cut itself lives in two of these folders, once per encoding, with the same structure:

| part | OnPair (Rust) | FSST (C++) |
|---|---|---|
| planning: alignment graph, minimum cut, probe cover | `third_party/onpair/src/search/substring/prefilter/` (`graph.rs`, `mincut.rs`, `cover.rs`, `plan.rs`) | `third_party/fsst/search/prefilter/` (`graph.hpp`, `mincut.hpp`, `cover.hpp`, `plan.hpp`) |
| scan: SIMD matchers, resolver, alignment walk, kernel choice | `.../prefilter/scan/` (`matcher/`, `resolver/`, `walk.rs`, `policy/`) | `.../prefilter/scan/` (`matcher/`, `resolver/`, `walk/`, `policy/`) |
| kernel sweeps behind the matcher figure | `.../prefilter/scan/bench/` | `.../prefilter/scan/bench/` |
| benchmark candidate | `candidates/simd_cut_onpair` | `candidates/simd_cut_fsst` |

The matchers have NEON, AVX2 and AVX-512 versions; which are compiled follows the target CPU
(`-C target-cpu=native` for Rust, `-march=native` for C++). In the end-to-end
systems, DuckDB evaluates `contains` on FSST strings with the FSST prefilter
(`third_party/duckdb/src/planner/filter/fsst_contains.cpp`, `src/common/vector/fsst_ops.cpp`
and the rule in `src/optimizer/rule/like_optimizations.cpp`), and Vortex stores strings as OnPair
in `third_party/vortex-onpair/encodings/onpair`.

Unmodified dependencies are fetched at the revisions in [pins.env](pins.env): upstream FSST,
FSST-LIKE and fmt (by CMake), duckdb-vortex (by `e2e/build.sh`) and the crates.io packages.
Each folder keeps its upstream licence.

## Tools

```sh
.venv/bin/pip install -r tools/dashboard/requirements.txt
.venv/bin/marimo run tools/dashboard/dashboard.py        # http://localhost:2718
```

The dashboard reads `results/bench.duckdb` and plots one run's GB/s against selectivity per
system. Stop it before a `bench run`, since DuckDB gives the loader no write lock while the
dashboard holds the file. `tools/bench-viz` builds a self-contained HTML explorer of result files.

## Adding a candidate or scanner

Implement `contract/lb_candidate.h`: in C or C++ as a directory with its own CMakeLists and a
small glue crate, in Rust as one crate against `lb-abi`. Then register it with one
feature-gated line in `harness/src/registry.rs` and one in `harness/Cargo.toml`.
`candidates/cpp_identity` and `scanners/memmem` show the two patterns.
