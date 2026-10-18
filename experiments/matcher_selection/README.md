# Needle suites for the paper

`matcher-selection` generates the SA + LCP needle suites every paper spec
reads, over the four 2^20-row columns of `config-1mi.toml`. It uses the
[shared SA + LCP generator](../../harness/src/gen/README.md) and blesses each
suite with the independent row oracle.

```sh
.venv/bin/python datasets/prepare.py --all                   # the four columns
./experiments/matcher_selection/generate.sh --per-cell 1     # fast; 10 medium, 20 full
```

`reproduce-neon.sh` and `reproduce-avx512.sh` run both steps.

## Query policy and outputs

Seed 42, seven byte-length buckets covering 1–256, and `--per-cell` unique
needles per length/selectivity cell (default 20). Selectivity is matching rows
over all rows; zero and one matching row have their own buckets. Zero-match
needles are observed substrings with one byte changed, checked absent from the
whole column. Needles are raw bytes and may be invalid UTF-8.

```text
generated/<dataset-id>/sa-lcp-v1-s42-n<per-cell>-<fingerprint>/
    suite.json         dataset identity, request and blessing metadata
    queries.jsonl      needles, generation witnesses and verified truth
    gen-report.json    exact bucket bounds, availability and generated counts
    preparation.json   checksum, index budget and preparation timings
```

The fingerprint covers the dataset checksum, generator version and request, so
a rerun reproduces the same suite and an existing one is kept unless `--force`
is given. `generated/` and the SA cache in `cache/` are gitignored.

## Index memory

Each column's index is admitted against a 16 GiB budget, estimated as
`16 × (payload bytes + rows) + 64 MiB`; `--list` prints each column's
requirement and `--index-memory-mib` overrides the budget. The cached
suffix/LCP arrays take about six bytes per symbol on disk.

## Checks

```sh
cargo test --locked -p matcher-selection
```
