# task_003 (task_6)

## Instruction (verbatim)

During the specified time range of March 6, 2021, from 06:00 to 06:30, there was a
single failure in the system. The component responsible for this failure and the
reason behind it remain unknown. Please determine the root cause component and the
root cause reason.

## Asked for

- root cause component
- root cause reason

(no occurrence time required)

## Window

Stated: 2021-03-06 06:00:00 – 06:30:00 (UTC+8, `ts`).
Materialised with margin: 2021-03-06 04:00:00 – 08:00:00.

## Notes

### Scripts

- `01_kpi_anomaly.py` — per (`cmdb_id`, `kpi_name`) window-vs-baseline z-scan over
  `metric_window.parquet`.
- `02_timelines.py` — bucketed timelines; separates periodic/global artifacts from the fault.
- `03_focus.py` — minute-level memory KPIs on Tomcat01, plus who-else checks.
- `04_confirm.py` — re-rank with confounders excluded; `log_service` and `metric_app` cross-check.

### Two confounders, both ruled out

1. **05:55 multi-host `DSKWrite` burst** — `Mysql01` 252.9, `Mysql02` 248.9,
   `apache01` 246.1, all in the *same* one-minute sample, against a ~10 baseline.
   Simultaneous across unrelated hosts and *before* the stated window: an
   infrastructure-level write (backup/flush), not a single-component fault. It was
   inflating the Mysql anomaly counts, so `04_confirm.py` excludes 05:50–06:00 from
   both the window and the baseline.
2. **`apache01` sustained `DSKRead` 400–2000** — runs across the full 04:00–08:00
   span, including the quiet hours. That is apache01's baseline, not an event.

With both removed, the ranking is unambiguous — the top four rows are all Tomcat01.

### Tomcat01, 06:20–06:24 (minute-level, `03_focus.py`)

| ts | NoCacheMemPerc | MEMUsedMemPerc | FreeMem MB | CacheMem MB | UserMem MB | JVM used % |
|---|---:|---:|---:|---:|---:|---:|
| 06:19 | 43.79 | 92 | 551 | 3828 | 7238 | — |
| 06:20 | 75.12 | — | 125 | 1812 | 7663 | — |
| 06:23 | 75.52 | — | 152 | 1753 | 7636 | — |
| 06:24 | 75.53 | 98 | — | — | — | — |
| 06:25 | — | 98 | 2787 | 1736 | 5001 | — |
| 06:26 | 41.92 | 64 | 2788 | 1736 | 5001 | — |

Non-cache memory goes from ~3410 MB to ~5851 MB (+2.4 GB of anonymous memory), free
memory collapses 551 → 125 MB, and the kernel evicts 2 GB of page cache to pay for
it. Held for five samples, released at 06:25; UserMem settles at 5001 MB, *below*
the pre-fault 7238 MB, because the evicted cache does not come back.

### Why not the other candidate reasons

- **high disk I/O read usage** — `DSKRead` does spike on Tomcat01 (b_avg 0.56 →
  373.6, z=86) but in a *single* sample at 06:20, at `DSKRTps` 2.167, i.e. ~2 reads
  per second. A read stressor holds high tps for the whole injection; this is the
  re-read after the page cache was evicted. Direction also disagrees: a read
  stressor *grows* CacheMem and leaves `NoCacheMemPerc` flat. Here CacheMem halved.
- **JVM OOM Heap / high JVM CPU load** — `JVMMemoryUsedPercent` on Tomcat01 goes
  37 → 32 → 25 across the event: heap usage *fell*. Tomcat01 logged 4 `gc` lines in
  06:00–06:40 versus 3 in the quiet 05:00–05:40; no `error`, no `catalina`. The
  pressure is OS-level, not in the heap.
- **high CPU usage** — `CPUCpuUtil` peaks at 30.25 against a 25.7 baseline. Real
  (z=21, the series is very tight) but a ~4.5-point move that tracks the memory
  event, not drives it.
- **network latency / packet loss** — those leave no KPI signature; this one leaves
  a large, localised, self-consistent memory signature, so there is nothing left
  for the trace analysis to explain.
- Application KPIs in `metric_app` stay at `sr` 100.0 throughout the window — the
  fault did not reach the service level, consistent with a five-minute
  memory squeeze on one host.

## Answer

- root cause component: **Tomcat01**
- root cause reason: **high memory usage**
