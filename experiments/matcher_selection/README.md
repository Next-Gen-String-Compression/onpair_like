# Matcher selection: query generation and fixed-cover benchmarks

This experiment prepares diverse CONTAINS workloads for comparing matcher
policies. It uses the [shared SA + LCP generator](../../harness/src/gen/README.md)
to cover length and selectivity buckets. There are no training/validation/test
splits. Preparation and matcher comparison are separate commands; neither fits
coefficients or modifies OnPair's planner.

## From a fresh clone

Run from the repository root, with a current stable Rust toolchain and Python
3.9 or newer:

```sh
python3 -m venv .venv
.venv/bin/pip install -r experiments/matcher_selection/requirements.txt

# Show columns and index requirements without downloading.
.venv/bin/python experiments/matcher_selection/prepare.py --list

# Prepare the same three datasets as optimize_prefilter (the default roster).
# Download/extract/ingest columns using datasets/sources.yaml.
# Builds the minimal Rust harness, without unrelated codec dependencies.
.venv/bin/python experiments/matcher_selection/prepare.py

# Generate suites and independently verify every needle against all rows.
./experiments/matcher_selection/generate.sh

# Numbered bucket grids in the supplied order, plus PDF and exact counts in CSV.
.venv/bin/python analysis/needle_coverage.py \
  experiments/matcher_selection/generated/clickbench-url-1m \
  experiments/matcher_selection/generated/amazon-title \
  experiments/matcher_selection/generated/dbpedia-abstract \
  --out results/needle-coverage
```

This writes `results/needle-coverage/coverage-1.png`, `coverage.pdf` and
`coverage.csv`. Add `--panels-per-page 1` to produce a separate PNG for each
suite. No temporary plotting scripts are needed.

With the pinned inputs and default generator settings, the three suites contain:

| Dataset | Rows | Unique needles | Of which negative | Canonical checksum |
|---|---:|---:|---:|---|
| ClickBench URLs | 1,000,000 | 1,276 | 140 | `xxh3:d164c1b5dbd6aff2` |
| Amazon book titles | 4,448,181 | 1,040 | 140 | `xxh3:3a0b57c7356a57a6` |
| DBpedia abstracts | 1,000,000 | 1,111 | 140 | `xxh3:09644aa73fc3e4b1` |

Both preparation and generation process exactly these three datasets by default.
To start with ClickBench only, pass `--dataset clickbench-url-1m` to both
commands. Existing downloads and prepared columns are reused by the shared
preparation script.

`config.toml` uses the same dataset IDs, paths and preparation recipes as
`optimize_prefilter`: ClickBench URLs (**1,000,000 rows**), Amazon book titles
and DBpedia abstracts. Query generation uses SA/LCP and length buckets spanning
1–256 bytes, independently of `optimize_prefilter`'s sampled 1–64-byte queries.
The SA indexes every row of each prepared column; it has no additional sample
or pilot limit. Other dataset recipes remain available in
`datasets/sources.yaml`, but are not selected by this experiment's default config.

Every configured recipe pins its canonical checksum; downloaded sources also
have SHA-256 pins. The dataset manifest and suite record the canonical identity.
When adding a recipe with `recorded-at-prepare`, use `datasets/prepare.py
--dataset <id> --update-checksums` and review the pins before publishing results.

## Index memory

The experiment's index budget is **16384 MiB (16 GiB)** per dataset, processed
sequentially. The admission estimate is:

```text
16 × (payload bytes + number of rows) + 64 MiB
```

This is a conservative workspace estimate, not a reservation or an OS memory
cap. The loaded Arrow dataset and extraction tools need additional memory.
Cached suffix/LCP arrays take about six bytes per encoded symbol on disk.

The largest measured requirement in this three-dataset roster is **5.11 GiB for
DBpedia abstracts**. The table uses canonical column sizes. The 16 GiB budget
is an admission limit, not memory allocated for every dataset; the actual
manifests are checked before generation:

| Dataset | Index admission budget (GiB) |
|---|---:|
| ClickBench URLs, 1M rows | 1.40 |
| Amazon book titles | 3.58 |
| DBpedia abstracts | 5.11 |

`prepare.py --list` uses the prepared manifest when available, otherwise the
recipe's approximate sizes. After preparation, obtain requirements from actual
sizes before generating any queries:

```sh
cargo run --locked --release -p matcher-selection -- --list
```

Generation preflights **all selected datasets** against the budget before
starting. Override it with `generate.sh --index-memory-mib <MiB>` or change the
config. The backend supports at most `i32::MAX` encoded symbols; exceeding that
limit is an error, never an implicit truncation.

## Query policy and outputs

Defaults are seed 42, seven byte-length buckets covering 1–256, and **20 unique
needles per length/selectivity cell**. Selectivity means matching rows divided
by all rows. Zero and one matching row have separate buckets; the remaining
disjoint buckets cover rare matches through 100%.

The zero-match bucket targets **140 negative needles**: 20 for each length
bucket, with up to 4000 mutation attempts per bucket. These are observed
substrings with one byte changed, checked absent from the whole column.
An incomplete negative bucket is reported as unresolved, not impossible.
For positive buckets, `available` counts all distinct eligible substrings;
an empty bucket with `available = 0` really has no such substring.

Selection is stratified, not uniform over all substrings. Needles are raw byte
strings and may be invalid UTF-8. Every suite is independent and contains no
duplicates. Other datasets, experiments or execution order do not change it;
the same needle may appear in separate suites.

```text
generated/<dataset-id>/sa-lcp-v1-s42-n20-<fingerprint>/
    suite.json         dataset identity, request and blessing metadata
    queries.jsonl      needles, generation witnesses and verified truth
    gen-report.json    exact bucket bounds, availability and generated counts
    preparation.json  checksum, index budget and preparation timings
```

The fingerprint includes the dataset checksum, generator version and complete
request. `--seed` or `--per-cell` creates a different suite. An exact repeat
requires `--force` to replace it. Experiment outputs and caches are gitignored.
The immutable SA cache can be shared across experiments without coupling their
query selection.

`generate.sh` always runs the independent row oracle and checks SA-derived
counts, buckets, uniqueness and mutation witnesses. The Rust command can be
invoked without `--bless` for preparation-only diagnostics; such suites cannot
pass the benchmark's correctness gate.

The optional coverage plot reads these reports without running queries or
regenerating needles. Each square is a length-bucket/selectivity-bucket pair
and displays its generated count. Hatching marks zero available positive
substrings; an orange outline marks an incomplete negative search. Passing
several roots or suite directories compares them on separate panels.

## Compare matchers

After preparation, clone the compatible SpiralDB/OnPair refactor and check out
the tested revision. Run these commands from the `onpair_like` repository root:

```sh
git clone --branch refactor/substring-search https://github.com/spiraldb/onpair.git ../onpair-matcher-selection
git -C ../onpair-matcher-selection checkout --detach c27d39782e63cb521ec4f62ad42016fc7c77dfb4

# Uses the same three datasets as preparation and generation by default.
.venv/bin/python experiments/matcher_selection/benchmark.py \
  --onpair ../onpair-matcher-selection \
  --out results/matcher-selection/first
```

The commit pin keeps the benchmark reproducible when the branch advances.
An existing checkout of that revision also works. Use a current stable Rust
toolchain, as for query generation; the run records the compiler version.
Add `--dataset clickbench-url-1m` to preparation, generation and benchmarking
to start with just one dataset.

`--onpair` is explicit: there is no fallback to the old pinned `onpair_spiral`
candidate. The launcher copies the local Rust source into an ignored build
directory and adds `adapter.rs` to expose eligible matcher choices. It does not
edit the OnPair checkout, replace matcher implementations, or change the
existing benchmark candidates. The snapshot checks production source hashes;
reusing a manually modified snapshot is an error. A different source revision
gets a different snapshot. This adapter currently targets the refactored
`ContainsScan` / `plan` / `scan` layout.

The runner compresses each complete prepared column once with seed 42,
16 dictionary bits and threshold 0.15, then builds the frequency index. For
each needle it prepares the production cover once and compares:

- `current`: normal `ContainsScan::scan`, including matcher selection;
- `table`, `eq_or`, `range`, `nibble_n8`: every eligible matcher forced on that
  same cover, using the production resolver and exact graph walker.

Vector packing follows the current plan and is held fixed across the forced
vector matchers. Results record its value; this first experiment isolates
matcher selection and does not optimize packing or cover selection.
Each variant must reproduce the independently blessed bitmap before timing.
Timing includes matcher setup and the complete scan/verification path, with
a reused output buffer. Compression, cover preparation, correctness checks,
and serialization are outside the timed region.

Defaults are **5 rounds**, at least **5 ms and 3 iterations** per variant per
round, with a warm-up immediately before timing. Query order and variant order
are deterministically shuffled. All queries are included; `--query-limit` is
an explicit smoke-test option recorded in the manifest. Use a quiet machine
and repeat runs before drawing conclusions from small differences.

The normal command requires exactly one generated suite for each selected
dataset. For another seed/quota or existing suites elsewhere, pass explicit
pairs instead of `--dataset`:

```sh
.venv/bin/python experiments/matcher_selection/benchmark.py \
  --onpair ../onpair-matcher-selection \
  --case path/to/prepared-column path/to/verified-suite \
  --out results/matcher-selection/another-run
```

Output directories must be new. Each run records source revisions and hashes,
the dependency lockfile, compiler and host information, dataset/suite checksums,
measurement settings, and all per-round timings. Results include:

```text
manifest.json       provenance and completion status
Cargo.lock          resolved benchmark dependencies
matcher-selection-bench  executable frozen for this run
<dataset>.jsonl     query facts, cover, correctness gate and timings
measurements.csv    tabular per-query/per-matcher results
summary.json        policy comparisons, per-bucket summaries and worst cases
SUMMARY.md          readable latency percentiles and comparisons
```

`report.py RUN_DIRECTORY` regenerates summaries and rejects incomplete runs.
It compares the current policy, Table, three exploratory shape rules, and the
fastest measured eligible matcher per query. The latter is a noise-sensitive
oracle bound; the shape rules are hypotheses evaluated on these same queries,
not fitted or validated production replacements. The current policy is unchanged.

## Interpreting the experiment

Balanced needle coverage does not guarantee balanced matcher inputs. Matcher
comparisons must also record normalized point/range counts, covered-token
frequency and ISA, and run eligible matchers on the **same fixed cover**. This
separates matcher selection from cover selection. The grid is a diagnostic
workload, not a model of production query frequencies.

## Checks

```sh
cargo test --locked -p matcher-selection
cargo test --locked -p lb-harness --no-default-features \
  --features cand-uncompressed,scan-memmem --lib --test gen
```

These cover exhaustive tiny-catalogue comparisons, row boundaries, uniqueness,
resource limits, cache identity, independent oracle verification, existing
sampled generation, and independence between dataset selections and settings.
