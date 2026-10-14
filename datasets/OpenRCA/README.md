# OpenRCA — Bank

Telemetry from [microsoft/OpenRCA](https://github.com/microsoft/OpenRCA) (MIT),
converted to parquet.

OpenRCA is an ICLR 2025 benchmark for root cause analysis: given a
natural-language query about a failure window, an LLM must sift a running
system's operational telemetry and name the failure's time, component and
reason. It ships **335 failures from three enterprise systems** with **over
68 GB of telemetry**. We are not doing root cause analysis — what this repo wants
is the haystack: `log_service.value` is 1.97 GB of real application log text for
the LIKE/CONTAINS matchers to scan.

| | |
|---|---|
| Download | Drive folder [`1wGiEnu4OkWrjPxfx5ZTROnU37-5UDoPM`](https://drive.google.com/drive/folders/1wGiEnu4OkWrjPxfx5ZTROnU37-5UDoPM) → `Bank.zip`, id `1enBrdPT3wLG94ITGbSOwUFg9fkLR-16R` |
| `Bank.zip` | 2,829,139,841 B, 95 entries, expands to 25 GB |
| Paper | Xu et al., ICLR 2025 — <https://openreview.net/forum?id=M4qNIzQYpd> |
| Upstream code | `vendor/OpenRCA` submodule (evaluator, RCA-agent baseline, task specs) — no data |

Drive is not content-addressed and serves anything this large behind a virus-scan
interstitial, so there is no immutable URL to pin the way `datasets/sources.yaml`
pins its entries. The script resolves the confirm token itself and logs the
sha256 it saw; numbers here are from the archive as served 2026-08-06 (members
dated 2024-10-02).

`Bank` is a bank's Java microservice deployment over **10 non-contiguous days** —
2021-03-04, 05, 06, 07, 09, 10, 12, 23, 24, 25. Components are pod-level
throughout (`Tomcat01`–`04`, `Mysql`, `Redis`, `apache`, `IG`, `MG`, `dockerA/B`);
upstream's FAQ notes Bank has no node/pod/container hierarchy, only horizontal
traces. 136 injected faults over 8 reasons (CPU/memory/disk/JVM-OOM/network); the
two network ones are invisible in KPIs and need parent/child span latency.

**Timezone.** Every wall-clock value upstream is **UTC+8** — `record.csv` row 1
pairs epoch `1614841020` (06:57:00 UTC) with datetime `2021-03-04 14:57:00`.
Reading the epochs as UTC shifts every fault by 8 h.

## Usage

```bash
python3 datasets/OpenRCA/download_and_transform.py              # Bank
python3 datasets/OpenRCA/download_and_transform.py --system Telecom
```

`--system` is the only option. Output goes to `data/bank/parquet/` and `data/` is
git-ignored. The zip and every extracted CSV — the day-sharded telemetry and the
two at the system root — are deleted once the parquet is written, leaving 2.8 GB
of parquet as the single representation; peak disk during the run is ~35 GB
(2.6 GB zip + 24.6 GB CSVs + 2.8 GB parquet).

Re-runs skip whatever is on disk, and short-circuit entirely once the raw CSVs
are gone. Rebuilding means re-downloading: delete `parquet/` and run again.

## Tables

One parquet per distinct CSV name, each the union of that CSV across all 10 days
in date order, zstd. Six and not five: `telemetry/*/metric/` holds **two** CSVs,
and `metric_app` (per-service app KPIs) and `metric_container` (long-form per-pod
infra KPIs, 497 distinct `kpi_name`) share no columns beyond `timestamp`.

| parquet | source | rows | parquet | CSV |
|---|---|---:|---:|---:|
| `log_service` | `telemetry/*/log/log_service.csv` | 21,356,870 | 564 MiB | 3.4 GiB |
| `metric_app` | `telemetry/*/metric/metric_app.csv` | 130,743 | 769 KiB | 5.8 MiB |
| `metric_container` | `telemetry/*/metric/metric_container.csv` | 12,218,094 | 38.7 MiB | 798 MiB |
| `trace_span` | `telemetry/*/trace/trace_span.csv` | 214,337,882 | 2.2 GiB | 20.4 GiB |
| `record` | `record.csv` | 136 | 3.2 KiB | 8.5 KiB |
| `query` | `query.csv` | 136 | 10.0 KiB | 55.5 KiB |

Parquet bytes are encoder-dependent (written by DuckDB 1.3); row counts are not.
`record` is the ground truth — do not feed it to anything meant to *solve*
OpenRCA. `query.task_index` is the task *type* (`task_1`…`task_7`), not an id.

Columns come through verbatim, names and types from DuckDB's sniffer. Telemetry
tables gain two:

- `day DATE` — from the `telemetry/{YYYY_MM_DD}/` directory. The *capture* day,
  not always the day of `ts`: `2021_03_04`'s first span is stamped
  `2021-03-03 23:59:59.628`, and `metric_app` files spill either side of midnight.
- `ts TIMESTAMP` — `timestamp` as UTC+8 wall clock, lining up with
  `record.datetime` and the windows in `query.instruction`. Naive, no zone. The
  raw `timestamp` is kept untouched beside it.

## Two upstream quirks the script handles

**1. `trace_span.timestamp` has no single unit.** `2021_03_04`–`03_12` store
13-digit epoch **milliseconds** (`1614787199628`); `03_23`–`03_25` store 10-digit
epoch **seconds** (`1616428798`). `log_service` and both metrics are seconds
throughout. `ts` branches on a 1e11 cutoff — 2021 is ~1.6e9 s / ~1.6e12 ms, three
orders of magnitude of headroom either way. Assuming one unit puts a quarter of
the spans in **1970**.

**2. `log_service.csv` defeats DuckDB's CSV sniffer.** `apache_access_log` rows
are RFC4180-quoted with doubled inner quotes:

```
ac1891…,1614831458,apache02,apache_access_log,"IPAddress ""POST /UOCP/base/ServiceTest6.json HTTP/1.1"" 200 391 ""-"" ""k6/0.29.0 (api_url 697220 0"
```

The sniffer settles on `escape=''` and rejects line 628,647 of the first file
with *"Value with unterminated quote found"*. Passing `quote='"', escape='"'`
reads `""` as one literal quote. `ignore_errors=true` would silently drop ~10.4 M
rows of access log.

## String-matching notes

`log_service.value`: 21,356,870 rows, **1,974,747,691 B** of UTF-8, mean 92.5 B,
max 20,672 B (a `catalina` Tomcat startup warning), 2,857,933 distinct. Against
`datasets/loghub/hdfs-v1.parquet`'s `message` it is the same shape at twice the
size but far more redundant:

| | OpenRCA Bank `value` | loghub HDFS `message` |
|---|---:|---:|
| rows | 21,356,870 | 11,175,629 |
| payload | 1.97 GB | 1.04 GB |
| mean length | 92.5 B | 92.6 B |
| distinct | 2,857,933 (13.4%) | 10,335,015 (92.5%) |

Near-identical mean with a 7× lower distinct count is the interesting part: HDFS
messages are nearly all unique because each carries a fresh block id, while these
are 99% two access-log formats whose rows repeat verbatim (`localhost_access_log`
10.8 M rows, `apache_access_log` 10.4 M, then `gc` 136 K, `slow` 52 K, `catalina`
12 K, `localhost` 648, `error` 162). Expect dictionary and repeat-aware schemes to
look much better here than on HDFS — which is why it is worth having both.

`log_name` and `cmdb_id` (7 and 8 distinct) are dictionary-degenerate.
`trace_span.trace_id`/`span_id` are a third shape: fixed-width 24 B and ~27 B
near-random ids, ~10.8 M distinct traces and 214,332,297 distinct spans.

```bash
bench ingest --source datasets/OpenRCA/data/bank/parquet/log_service.parquet \
  --format parquet --column value --id openrca-bank-log
```

Not in `datasets/sources.yaml`: with no pinnable URL it stays a manual recipe,
like the full ClickBench column.
