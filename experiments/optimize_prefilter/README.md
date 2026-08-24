# Optimize-prefilter experiment

This self-contained experiment generates deterministic, `contains`-only query
suites for studying SpiralDB/OnPair prefiltering. Its generator, configuration,
scripts, fixture, and tests live together under `experiments/optimize_prefilter`.

The default experiment covers:

- ClickBench URLs (`clickbench-url-1m`)
- Amazon Books product titles (`amazon-title`)
- DBpedia English short abstracts (`dbpedia-abstract`)

Any canonical harness dataset (`data.arrow` plus `manifest.json`) can be passed
with `--dataset`, so the generator itself has no dataset-specific logic.

## What is generated

For every needle length from 1 through 64, the positive selectivity space is
split into four logarithmic bands per decade from one matching row through 2%,
then bands ending at 3%, 5%, 8%, 12%, 20%, 35%, 50%, 75%, and 100%. An exact
zero-match band is separate. Bands use integer row-count boundaries, so no
query can move between bands due to floating-point rounding.

The exhaustive profile requests 32 unique needles in every positive sub-2%
cell, 16 in every other positive cell, and 16 zero-match needles at each length.
Candidate discovery combines row-uniform real substrings, occurrence-uniform
real substrings, and deterministic prefix/middle/suffix anchors. Every sampled
candidate is counted over every row. Synthetic mutations are permitted only
for the zero-match band, use only bytes observed somewhere in the dataset, and
survive only after an exact absence check. This prevents a negative probe from
being made trivial by a globally absent byte.

Discovery is bounded rather than an enumeration of every distinct substring.
That distinction matters: a long natural-language dataset contains billions
of distinct windows. Reports therefore use `not_observed`, never `impossible`,
for an empty cell. Increase `sampling.candidate_draws` or
`sampling.catalog_entries_per_cell` in [config.toml](config.toml) when the gap
report shows a cell that needs a deeper search.

Changing `--seed` does not rebuild the expensive catalogue. It ranks the
cached candidates with the dataset checksum, selection seed, and exact needle
bytes, yielding a different reproducible subset. Duplicate decoded needle
bytes are rejected globally within each dataset suite.

## Workload profiles

Profiles are named sections in [config.toml](config.toml). Replication depth
and selectivity scope are independent: omitting `max_selectivity_percent`
covers the full space, while `max_selectivity_percent = 2.0` admits only exact
selectivity `< 2%` (including zero-match queries).

The checked-in profiles are:

| Profile | Scope | Replicates zero / low / other | Approximate seed-42 queries |
|---|---|---:|---:|
| `prototype` (default) | Full space | 1 / 1 / 1 | 3,804 |
| `substantial` | Full space | 2 / 4 / 2 | 13,570 |
| `exhaustive` | Full space | 16 / 32 / 16 | 101,576 |

The estimates are totals across the current three catalogues; exact counts are
reported after generation. The default seed-42 profile emits 1,058 Amazon,
1,527 ClickBench, and 1,219 DBpedia queries, one per observed cell, all 64
needle lengths present in each.

Every profile covers 0% through 100%. Three `prefilter-*` profiles that capped
selectivity at 2% used to ship here and were removed: 2% is the region where
prefiltering wins, so a suite built from them showed how *well* it does and
never where it stops paying — on these catalogues they admitted no query at or
above 2% at all. Depth and scope are no longer independent knobs; pick a profile
for how many replicates you want, not for which half of the space you see.

List the authoritative configured profiles with:

```sh
cargo run -p optimize-prefilter -- profiles
```

A profile may still set `max_selectivity_percent` to study one narrow slice, but
it costs the upper half of the space and no shipped profile does it. Profiles
share the same catalogue cache, so changing replication does not repeat
candidate discovery.

Generation is cheap and cached; the benchmark is what costs wall clock. A full
catalogue build from scratch is roughly a minute per million rows per dataset,
paid once, and every later profile and seed reuses it. Changing
`log_bands_per_decade` or `low_selectivity_cutoff` does redefine the bands and
so does force rediscovery.

### Grid cells versus benchmark queries

`coverage.csv` has one row for every `(selectivity band, needle length)` cell.
It is a coverage report, not the benchmark input. A covered cell emits up to
its replicate quota, and every emitted query is written to `queries.jsonl` and
executed by the benchmark. Thus roughly 2,000 grid cells can produce anything
from roughly 1,000 prototype queries to 30,000–40,000 exhaustive queries for a
dataset.

This is a balanced diagnostic workload: it deliberately gives rare and common
selectivities, and every length, comparable representation. Its needles are
dataset-derived (except exact-negative probes), but its frequencies are not a
claim about a production query mix. Use it to map the performance surface and
identify crossover points; use a trace-derived suite or weighted aggregation
when estimating one application's end-to-end latency.

Four logarithmic bands per decade below 2% put adjacent boundaries about
1.78× apart. That is already fine-grained relative to the high-selectivity
bands. `log_bands_per_decade = 8` is available for a focused study of a narrow
crossover, but it approximately doubles this part of the benchmark.

## Prepare the initial datasets

The repository does not commit dataset artifacts. Build the harness and
materialize the three pinned sources once (the current Amazon Books download is
4.94 GB):

```sh
python3 -m venv .venv
.venv/bin/pip install -r datasets/requirements.txt
cargo build --release -p lb-harness --bin bench --no-default-features
BENCH_BIN=target/release/bench .venv/bin/python datasets/prepare.py \
  --dataset clickbench-url-1m \
  --dataset amazon-title \
  --dataset dbpedia-abstract
```

## Generate and inspect queries

```sh
./experiments/optimize_prefilter/generate.sh --profile prototype --seed 42
./experiments/optimize_prefilter/generate.sh --profile substantial --seed 42
./experiments/optimize_prefilter/generate.sh --profile exhaustive --seed 43
```

Omitting `--profile` selects `default_profile`. To run one or more arbitrary
prepared datasets, repeat `--dataset`:

```sh
./experiments/optimize_prefilter/generate.sh --profile prototype --seed 42 \
  --dataset datasets/my-column \
  --dataset /absolute/path/to/another-column
```

Each `generated/<profile>/seed-N/<dataset>/` directory is an ordinary,
already-blessed harness suite and contains:

- `queries.md`: the complete human-readable catalogue, sorted by exact
  selectivity, then needle length;
- `queries.csv`: the same list with exact bytes in base64;
- `coverage.csv`: every band/length cell, including gaps;
- `gen-report.json`: methodology, dataset binding, bands, and coverage;
- `suite.json` and `queries.jsonl`: benchmark inputs with canonical bitmap
  truth.

Cache and generated folders are ignored because they are reproducible and can
be large.

## Benchmark and open the explorer

Generation writes a normal harness `benchmark.toml` pairing every dataset with
its suite. Run it and build the self-contained Benchmark Explorer with:

```sh
./experiments/optimize_prefilter/benchmark.sh --profile prototype --seed 42
./experiments/optimize_prefilter/benchmark.sh --profile substantial --seed 42
```

The resulting `results/optimize_prefilter/<profile>/seed-42/explorer.html`
plots exact selectivity or needle length and can switch datasets, strategies,
scales, and aggregations without regenerating the benchmark. The run spec
compares these three logical paths:

- uncompressed `memmem-hay`: one search over the concatenated payload;
- OnPair `decode / memmem-hay`: bulk decode, then one search over the decoded
  concatenated payload with row-boundary rejection — the baseline the shipped
  profitability policy was calibrated against;
- OnPair `pf_memmem`: compressed-domain prefilter, then decode and `memmem`
  verification of surviving rows.

Three is deliberate. Per-row `memmem`, the unprefiltered `kmp` DFA and the FSST
decode paths all answer adjacent questions, and each one multiplies every cell
in the matrix: nine series over 6,402 queries was 57,618 measured cells and, at
`min_millis = 50`, close to an hour of floor before warmup. Three series over
3,804 queries is 11,412. Add candidates back in `benchmark_spec` when the
question needs them.

The OnPair paths run with one 16-bit dictionary; comparing dictionary widths is
a separate question and doubles every cell. Edit the
benchmark section in `config.toml` to change those configurations or timing
settings. SpiralDB retains its compact dictionary and expands it to one
`WideDictionary` inside every full-column decode, so that expansion is included
in bulk-decompression latency; its random-access survivor verification stays on
the compact dictionary. FSST imports and retains its small read-only decoder at
build. The explorer exposes harness-measured decode-only throughput separately
from end-to-end decode-plus-scan throughput.

The benchmark script builds only the three selected candidate features and the
scanner module providing `memmem` and `memmem-hay`, so unrelated candidate
submodules are not prerequisites for this experiment.

## Tests

Run the experiment's unit and end-to-end fixture tests with:

```sh
cargo test -p optimize-prefilter
```

The local `tests/fixtures/mini.csv` fixture exercises exact bands, global
needle uniqueness, ordering, truth generation, report generation, cache reuse,
and harness loading without requiring any downloaded dataset.
