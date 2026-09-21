# Matcher selection: reproducible query preparation

This experiment prepares diverse CONTAINS workloads for comparing matcher
policies. It uses the [shared SA + LCP generator](../../harness/src/gen/README.md)
to cover length and selectivity buckets. There are no training/validation/test
splits. Matcher timing and policy comparison are the next stage; this command
does not fit coefficients or modify OnPair's planner.

## From a fresh clone

Run from the repository root, with a current stable Rust toolchain and Python
3.9 or newer:

```sh
python3 -m venv .venv
.venv/bin/pip install -r experiments/matcher_selection/requirements.txt

# Show columns and index requirements without downloading.
.venv/bin/python experiments/matcher_selection/prepare.py --list

# Reproduce the three-dataset coverage figure.
# Download/extract/ingest columns using datasets/sources.yaml.
# Builds the minimal Rust harness, without unrelated codec dependencies.
.venv/bin/python experiments/matcher_selection/prepare.py \
  --dataset clickbench-url-1m --dataset amazon-title --dataset dbpedia-abstract

# Generate suites and independently verify every needle against all rows.
./experiments/matcher_selection/generate.sh \
  --dataset clickbench-url-1m --dataset amazon-title --dataset dbpedia-abstract

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

To start with ClickBench only, select just `--dataset clickbench-url-1m` in
both preparation and generation. To add the fourth verified workload, include
`--dataset msmarco-query` in both commands. Omit all `--dataset` arguments to
prepare the complete ten-column roster; it includes large downloads,
particularly MS MARCO URLs and Amazon metadata. No datasets are silently
skipped. Existing downloads and prepared columns are reused by the shared
preparation script.

`config.toml` lists ten columns from Amazon, MS MARCO, TPC-H, DBLP, ClickBench and
DBpedia. Every ID has a recipe in `datasets/sources.yaml`. ClickBench uses the
same pinned **1,000,000-row** input as `optimize_prefilter`. The SA indexes every
row of each prepared column; it has no additional sample or pilot limit.

Every configured recipe pins its canonical checksum; downloaded sources also
have SHA-256 pins. The dataset manifest and suite record the canonical identity.
DBLP uses the archived [December 2025 snapshot](https://doi.org/10.4230/dblp.xml.2025-12-01).
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

The largest measured requirement is **10.49 GiB for DBLP titles**. The table
uses canonical column sizes; MS MARCO URLs retain the existing recipe estimate.
The 16 GiB setting covers these sizes, with the actual manifests checked before
generation:

| Dataset | Index admission budget (GiB) |
|---|---:|
| ClickBench URLs, 1M rows | 1.40 |
| Amazon book titles | 3.58 |
| DBpedia abstracts | 5.11 |
| MS MARCO queries | 0.61 |
| MS MARCO URLs (estimated) | 3.24 |
| TPC-H product names, SF10 | 1.07 |
| TPC-H customer comments, SF10 | 1.71 |
| TPC-H customer addresses, SF10 | 0.64 |
| DBLP titles | 10.49 |
| DBLP authors | 7.25 |

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

## Next experiment

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
