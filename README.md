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
spec hash).

## Reproducing everything

```sh
./reproduce.sh all    # build → datasets → suites → run (stages runnable alone)
```

`datasets/sources.yaml` pins every dataset (URL, sha256, license, canonical
checksum); `bench gen --seed 42` makes every query suite a pure function of
the dataset checksum; every run records its spec hash + environment. See the
header of [reproduce.sh](reproduce.sh) for stages and wall-clock estimates.

## Layout

| path | what |
|---|---|
| `contract/` | the C-ABI contract — the cross-language source of truth |
| `abi/` | Rust mirror of the contract (`lb-abi`) |
| `harness/` | Rust harness: CLI (`bench`), formats, loader, oracle, gate, timing, results |
| `candidates/` | storage schemes: `uncompressed` (Rust baseline), `cpp_identity` (identity/memcpy C++ smoke), `gate_canary` (proves the correctness gate fires), `lz4` / `zstd` (general block codecs, decode-then-eval), `fsst` (string-native FSST, decode baseline), `fsst_like` (FSST compressed-domain LIKE matching, DaMoN'26), `onpair` (the paper's codec: decode + compressed-domain automata), `onpair_spiral` (SpiralDB's Rust OnPair: contains via `pf_kmp` / `pf_memmem` prefilter+verify, and `kmp` un-prefiltered baseline) |
| `scanners/` | pluggable uncompressed eval kernels: `memmem` (Rust `memchr`), `libc` (`memmem(3)`), `cpp_std_find` (C++ `std::search`), `stringzilla` (SIMD), `classics` (BNDM/KMP/Boyer–Moore–Horspool), `multi` (Aho–Corasick + Teddy, for `contains_any`), `composed` (prefilter-ablation scanners). `ac_cpp` (the author's C++ Aho–Corasick) is parked — kept for reference, excluded from the build |
| `suites/` | query suites (`suite.json` + `queries.jsonl`, truth blessed in) — curated, or swept via `bench gen` (seed-deterministic grid + `gen-report.json` coverage matrix) |
| `specs/` | run specs: candidates × configs × scanners × datasets × chunk sizes |
| `datasets/` | `sources.yaml` (pinned URLs + sha256 + licenses + canonical checksums) + `prepare.py` (download → extract → ingest → verify); artifacts and raw downloads are gitignored |

## Adding a candidate or scanner

Implement `contract/lb_candidate.h` (C/C++: own directory + CMakeLists +
copy-paste glue crate; Rust: one crate against `lb-abi`), then register it
with one feature-gated line in `harness/src/registry.rs` and one line in
`harness/Cargo.toml`. See `candidates/cpp_identity` and `scanners/memmem`
as the two patterns.
