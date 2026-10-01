# Bank — RCA agent brief

You are the diagnostic agent for OpenRCA's **Bank** system: a bank's Java
microservice deployment, 10 non-contiguous days of March 2021. Each task gives you
one natural-language query about a failure window; you must name the failure's
**occurrence time**, **component** and **reason** — whichever of the three the
query asks for.

All paths below are relative to the repository root,
`/Users/paul/workspace/onpair_like`. Telemetry is in
`datasets/OpenRCA/data/bank/parquet/`; your working root is
`datasets/OpenRCA/local_agent/bank/out/`.

## Do not read the answers

- **`parquet/record.parquet`** is the injected-fault ledger — the full ground truth
  for all 136 failures. Never read it.
- **`parquet/query.parquet`** has a third column, `scoring_points`, holding the
  expected answer. You read this file exactly once, through the projection in the
  next section, and never again.
- **`vendor/OpenRCA/rca/archive/*.csv`** are recorded trajectories with a
  `groundtruth` column. Do not read them.

If you ever find yourself about to `SELECT *` from anything under `data/bank/`
that is not a telemetry table, stop.

## Build the task list once

Run this first, before any diagnosis. It projects away `scoring_points` and
writes the only task file you are allowed to read afterwards:

```bash
duckdb -c "
COPY (
  SELECT row_number() OVER () AS task_id, task_index, instruction
  FROM 'datasets/OpenRCA/data/bank/parquet/query.parquet'
) TO 'datasets/OpenRCA/local_agent/bank/out/tasks.parquet';
"
```

This is the one file you are allowed to write out — a view would leave
`query.parquet` reachable, and dropping the column is the point. Everything else
uses views; see below.

From here on, `out/tasks.parquet` is the task source. 136 rows, `task_id` 1–136.
Read one task like this:

```bash
duckdb -c "SELECT instruction FROM 'datasets/OpenRCA/local_agent/bank/out/tasks.parquet' WHERE task_id = 7;"
```

`task_index` is only the task *type* (`task_1`…`task_7`) — which of the three
fields the query asks for. The instruction text says the same thing in prose.

## One folder per task

Work strictly task by task, in ascending `task_id`. Finish one before starting the
next; never analyse two windows in one script.

For each task, create `out/task_<NNN>/` (zero-padded, e.g. `out/task_007/`) and
put **everything** for that task in it:

```
out/task_007/
  task.md         the instruction, copied in verbatim, plus your notes as you go
  task.duckdb     the database holding this task's views
  queries.csv     the DuckDB query log for this task only (see below)
  *.py            every script you write for this task
  answer.json     the final answer
```

Nothing for task 7 is written outside `out/task_007/`, and no script reads another
task's folder. `out/tasks.parquet` is the one shared file.

## Views, not extracts

**Never write an intermediate dataset.** No `COPY … TO`, no `CREATE TABLE AS`, no
`.parquet` or `.csv` of a filtered slice. The parquet in `data/bank/parquet/` is
the only copy of the data that exists, and every query you write must read from it.

Narrow the window with a **view** instead. Views store the query, not the rows, so
a later query still reaches the real table:

```sql
CREATE OR REPLACE VIEW mc AS
SELECT * FROM '/Users/paul/workspace/onpair_like/datasets/OpenRCA/data/bank/parquet/metric_container.parquet'
WHERE ts BETWEEN '2021-03-04 14:00:00' AND '2021-03-04 15:30:00';
```

Views live in `task.duckdb` in the task folder, so they persist across scripts and
are reachable from the shell (`duckdb out/task_007/task.duckdb -c "…"`). Use
**absolute paths** in view definitions — a view keeps the path text verbatim and
breaks when queried from a different working directory. For composition inside a
single query, a CTE is enough; a view is for what you reuse across queries.

This costs a rescan per query and that is accepted. DuckDB's parquet reader prunes
row groups on `ts` and `day` and reads only the projected columns, so a windowed
scan of `metric_container` touches a small fraction of the file. If a query on
`trace_span` is genuinely too slow, narrow the projection and the predicate — do
not fall back to writing a slice out.

Take a margin either side of the stated window in the view: the fault is injected
before its KPI signature is visible, and the recovery matters too.

## DuckDB does the work

Every read, filter, aggregate and join happens in DuckDB. SQL is the expected
idiom — write it directly, don't wrap it in a dataframe API. Concretely:

- No `pandas.read_csv` / `read_parquet`, no `pyarrow` scans, no iterating rows in
  Python. `trace_span.parquet` is 2.2 GiB and `log_service.parquet` is 21.4 M
  rows; loading them into Python is not a viable plan.
- Anything Python does after the query — filtering, string matching, grouping,
  windowing, joining — belongs in the query instead. Python is a host for the
  connection and for printing results, nothing more.
- One-off exploration: `duckdb -c "..."` at the shell. Anything you want to keep
  or re-run: a script in the task folder.

## Every query is logged, per task

Each Python script starts with exactly this, before any other data access. The
script lives in the task folder, so the log lands beside it:

```python
import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect(TASK / "task.duckdb")
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")
```

Then run everything on `con`. Shell one-liners are logged the same way — prepend
the call in the same `-c` string, or they are not recorded:

```bash
duckdb datasets/OpenRCA/local_agent/bank/out/task_007/task.duckdb -c "
  CALL enable_logging(storage_path = 'datasets/OpenRCA/local_agent/bank/out/task_007/queries.csv');
  SELECT cmdb_id, count(*) FROM mc GROUP BY 1;"
```

Only one process may hold `task.duckdb` open for writing, which is another reason
tasks run strictly one at a time.

Rules that keep the log complete:

- **One connection per script.** A second `duckdb.connect()` without the
  `enable_logging` call is silently unlogged.
- **Never call `disable_logging()`.** Never point `storage_path` at `/tmp` or
  outside the current task folder.
- The log appends across runs and writes its header once. Leave it alone — do not
  truncate, rotate, or hand-edit it.
- What Python computes is invisible to the log. That is the reason the previous
  section is a hard rule and not a style preference.

Read a log back with DuckDB like anything else:

```sql
SELECT timestamp, message
FROM read_csv('datasets/OpenRCA/local_agent/bank/out/task_007/queries.csv')
WHERE type = 'QueryLog' ORDER BY timestamp;
```

`enable_logging` needs a recent DuckDB: verified working on Python `duckdb` 1.5.5
and CLI 1.5.4, and **absent** from the Python 1.3.0-dev build that may be first on
`PATH`. If the call raises `CatalogException: Table Function with name
enable_logging does not exist`, print `duckdb.__version__` and upgrade before
continuing — do not proceed unlogged.

## Tables

`datasets/OpenRCA/data/bank/parquet/`:

| parquet | rows | columns |
|---|---:|---|
| `log_service.parquet` | 21,356,870 | `log_id`, `timestamp`, `cmdb_id`, `log_name`, `value`, `day`, `ts` |
| `metric_app.parquet` | 130,743 | `timestamp`, `rr`, `sr`, `cnt`, `mrt`, `tc`, `day`, `ts` |
| `metric_container.parquet` | 12,218,094 | `timestamp`, `cmdb_id`, `kpi_name`, `value`, `day`, `ts` |
| `trace_span.parquet` | 214,337,882 | `timestamp`, `cmdb_id`, `parent_id`, `span_id`, `trace_id`, `duration`, `day`, `ts` |

Days present: 2021-03-04, 05, 06, 07, 09, 10, 12, 23, 24, 25.

**Use `ts`, not `timestamp`.** `ts TIMESTAMP` is the raw epoch already converted
to **UTC+8** wall clock, which is the timezone every window in the query text and
every answer is expressed in. The raw `timestamp BIGINT` is kept beside it and is
a trap: `trace_span` stores milliseconds on 03-04 through 03-12 and *seconds* on
03-23 through 03-25, so a single divide-by-1000 puts a quarter of the spans in
1970. `ts` has that already resolved.

`day DATE` is the capture directory, not the day of `ts` — files spill either
side of midnight (`2021_03_04`'s first span is stamped `2021-03-03 23:59:59.628`).
Filter on `ts` for correctness; add a `day` predicate too when it lets DuckDB skip
row groups.

Per-table notes:

- `metric_container` is long-form: one row per (`ts`, `cmdb_id`, `kpi_name`),
  497 distinct `kpi_name` covering CPU, memory, disk, network and JVM. This is
  the primary signal.
- `metric_app` is per-service application KPIs — `rr` (request rate), `sr`
  (success rate), `cnt`, `mrt` (mean response time), `tc` (service name). No
  `cmdb_id`; it does not localise to a component on its own.
- `trace_span` is horizontal only — Bank has no node/pod/container hierarchy.
  Parent/child latency (`duration` of a span minus its children's) is the only
  way to see the two network reasons, which leave no KPI signature.
- `log_service.value` is 99% two access-log formats. Counts by `log_name`:
  `localhost_access_log` 10,798,833 · `apache_access_log` 10,357,651 · `gc`
  135,871 · `slow` 51,627 · `catalina` 12,078 · `localhost` 648 · `error` 162.
  The diagnostic content is in the small tail, not the access logs.

## How to diagnose

1. Anomalous KPIs in `metric_container` for the queried window, per `cmdb_id`.
   Compare against the same KPI's own baseline outside the window — absolute
   values mean little.
2. `trace_span` when metrics are clean, or when a latency pattern needs
   attributing to a specific component; required for the two network reasons.
3. `log_service` to decide *which* reason, when several KPIs on a component look
   faulty or the metric evidence is ambiguous. `gc`, `catalina`, `error` and
   `slow` are where JVM, OOM and disk faults surface.

The root cause time is the fault-injection moment, which precedes the KPI peak —
report the onset, not the worst point.

## Answer

Components and reasons must be chosen from these lists; there is no "unknown".

**Reasons:** high CPU usage · high memory usage · network latency · network
packet loss · high disk I/O read usage · high disk space usage · high JVM CPU
load · JVM Out of Memory (OOM) Heap

**Components:** apache01 · apache02 · Tomcat01 · Tomcat02 · Tomcat03 · Tomcat04 ·
MG01 · MG02 · IG01 · IG02 · Mysql01 · Mysql02 · Redis01 · Redis02

Write `out/task_<NNN>/answer.json` with only the fields the query asked for. One
numbered entry per failure — more than one only if the query says the failure
count is unknown or greater than one.

```json
{
    "1": {
        "root cause occurrence datetime": "%Y-%m-%d %H:%M:%S",
        "root cause component": "...",
        "root cause reason": "..."
    }
}
```
