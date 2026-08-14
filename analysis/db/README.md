# `results/bench.duckdb` — the queryable result store

`results/<group>/<run>/results.jsonl` stays the source of truth (DESIGN.md §9).
This DB is a derived index over it: five dimensions, one fact table, one view.
Delete the file and rebuild whenever you like.

```sh
.venv/bin/python analysis/db/load.py --all        # (re)load every run under results/
.venv/bin/python analysis/db/load.py results/sqlstorm   # one run
duckdb results/bench.duckdb
```

`bench run` calls the loader itself, so a finished run is already in the DB.
Loading is idempotent — a run whose `results.jsonl` is unchanged is a no-op, and
one whose file changed has its facts replaced.

## The model

| table | grain | holds |
|---|---|---|
| `platform` | machine | hostname, arch, os, cpu_model, cpu_features, cores |
| `run` | one `bench run` | timestamps, git commit, toolchain, pinning, governor, `warmup`/`min_iters`/`min_millis`, `is_latest` |
| `dataset` | column artifact | `num_rows`, `payload_mb`, `raw_bytes`, length stats |
| `system` | the compared unit | `label`, candidate, version, config, strategy, scanner |
| `query` | one blessed query | `op`, `needle`, `needle_len`, `selectivity`, `match_count` |
| `result` | one measurement | speed + compression + evaluation domain + prefilter attribution |

**`v`** joins all of them. Query `v`; the tables are for when you need a column
it doesn't carry.

Compression is copied onto every `result` row from the build row of the same
run × candidate × config × dataset × chunk size, so ratio and speed sit
side by side with no join.

## Seven queries

Ratio against speed — the two-axis figure:

```sql
SELECT label, round(median(ns_per_value), 2) AS ns_value,
       round(any_value(compression_ratio), 2) AS ratio
FROM v
WHERE run_name = 'clickbench-url-1m-contains' AND is_latest
  AND status = 'ok' AND chunk_rows = 122880
GROUP BY label ORDER BY ns_value;
```

Latency against selectivity, one candidate against the baseline:

```sql
SELECT CASE WHEN selectivity = 0 THEN 'no match'
            WHEN selectivity < 1e-3 THEN '<1e-3'
            WHEN selectivity < 1e-2 THEN '<1e-2' ELSE '>=1e-2' END AS band,
       round(median(ns_per_value) FILTER (WHERE label = 'onpair/compressed'), 2) AS onpair,
       round(median(ns_per_value) FILTER (WHERE label = 'uncompressed_memmem/memmem'), 2) AS baseline
FROM v
WHERE run_name = 'clickbench-url-1m-contains' AND is_latest
  AND status = 'ok' AND chunk_rows = 122880
GROUP BY band ORDER BY band;
```

Needle-length sweep:

```sql
SELECT label, needle_len, round(median(ns_per_value), 2) AS ns_value, count(*) AS n
FROM v
WHERE run_name = 'clickbench-url-1m-contains-l8l32' AND is_latest AND status = 'ok'
GROUP BY label, needle_len ORDER BY label, needle_len;
```

The same system on different machines. DESIGN.md §9 forbids merging
measurements from different machines into one number, so compare them as rows:

```sql
SELECT hostname, arch, round(median(ns_per_value), 2) AS ns_value, count(*) AS n
FROM v
WHERE label = 'uncompressed_memmem/memmem' AND dataset = 'clickbench-url-1m'
  AND op = 'contains' AND status = 'ok' AND chunk_rows = 122880
GROUP BY hostname, arch ORDER BY ns_value;
```

Prefilter attribution (DESIGN.md §10):

```sql
SELECT label, round(avg(prune_rate), 3) AS prune,
       median(scan_ns) AS scan_ns, median(decode_ns) AS decode_ns,
       median(setup_ns) AS setup_ns
FROM v
WHERE run_name = 'clickbench-url-1m-contains' AND is_latest
  AND status = 'ok' AND prune_rate IS NOT NULL
GROUP BY label ORDER BY prune DESC;
```

Where a `dict_*` system's speedup comes from — the dedup it bought against
the change in its engine's own per-value cost:

```sql
SELECT label,
       round(median(ns_per_value), 2)        AS ns_value,
       round(median(ns_per_domain_value), 2) AS ns_domain_value,
       round(median(dedup_factor), 1)        AS dedup
FROM v
WHERE run_name = 'clickbench-url-1m-contains' AND is_latest
  AND status = 'ok' AND chunk_rows = 122880
GROUP BY label ORDER BY ns_value;
```

`ns_value` is the headline, `dedup` is the part that came from evaluating
fewer values, `ns_domain_value` is the part that came from the engine — and
on every row `ns_value = ns_domain_value / dedup`.

Run health — what a number is worth before you quote it:

```sql
SELECT run_name, hostname, min_iters, pinning_effective, count(*) AS n,
       count(*) FILTER (WHERE status = 'ok' AND NOT gate_ok) AS gate_fail,
       count(*) FILTER (WHERE status <> 'ok') AS not_run,
       round(max(stddev_ns / nullif(mean_ns, 0)), 3) AS worst_rel_stddev
FROM v WHERE is_latest GROUP BY ALL ORDER BY run_name;
```

## Five things that will bite you

- **`chunk_rows`** is a build-time knob from the spec (`chunk_rows = [0, 122880]`;
  0 = the whole column as one chunk). It changes footprint *and* latency, and
  several runs measure both values — so filter it, usually to `122880`.
  Forgetting to is how you get a median over two different configurations.
- **`is_latest`**: rerunning a spec into the same directory adds a new `run` row
  (the DB keeps history the overwritten `results.jsonl` lost). `WHERE is_latest`
  is the everyday filter.
- **`status <> 'ok'`** is not an error: a large share of measurements are
  `unsupported`, an op a kernel doesn't implement. `gate_ok` is `false` for
  those, so `WHERE gate_ok` silently excludes them — which is usually what you
  want, but say so deliberately.
- **The phase columns are single-shot *timings*.** `setup_ns` / `decode_ns` /
  `scan_ns` come from one instrumented pass while `total_ns` is a median over
  `samples` iterations. Never divide one by the other; the `*_ns_origin`
  columns on `result` say who held the clock (`harness` at a pipeline joint vs
  `self_reported` by the module). The instrumented *counters* —
  `eval_domain`, `eval_domain_matches`, `prefilter_candidates` — are a
  different animal: they are structural, identical in every sample, so
  `ns_per_domain_value` and `verify_ns_per_survivor` divide the median by
  them legitimately.
- **`eval_domain` is NULL in the table and coalesced in the view.** The
  `dict_*` systems evaluate the predicate once per *unique* value and scatter
  to rows, so they declare a reduced domain; everything else leaves it unset,
  meaning one value per row. `v` substitutes `num_rows` for you, which is why
  `dedup_factor` is exactly `1.0` rather than NULL for the row-wise systems, and
  `dedup_factor > 1` selects the rows that declared a reduced domain.

## Two names that mean different things

- `gbps` divides by `payload_bytes`, the harness's own throughput denominator.
- `compression_ratio` divides `raw_bytes` (= `payload + 8·(rows+1)`, the
  canonical view size that puts `uncompressed` at exactly 1.0) by
  `compressed_bytes`.

They differ by ~9% on clickbench. Don't derive one from the other.
