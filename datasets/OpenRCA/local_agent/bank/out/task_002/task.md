# task_002 (task_6)

## Instruction (verbatim)

On March 4, 2021, between 18:00 and 18:30, there was a single failure observed in
the system. The exact component that caused this failure is unknown, and the reason
behind the failure is also undetermined. Your task is to identify the root cause
component and the root cause reason for this failure.

## Asked for

- root cause component
- root cause reason

(no occurrence time required)

## Window

Stated: 2021-03-04 18:00:00 – 18:30:00 (UTC+8, `ts`).
Materialised with margin: 2021-03-04 17:00:00 – 19:30:00.

## Notes

### 01 — KPI anomaly scan (`01_kpi_anomaly.py`)

Window 18:00–18:35 vs baseline (rest of 17:00–19:30), per (`cmdb_id`, `kpi_name`),
|z_max| > 4. Top of the ranking:

| cmdb_id | kpi | baseline avg | window max | z_max |
|---|---|---:|---:|---:|
| Mysql01 | `LOCALDISK-sdb_DSKRead` | 0.0023 | 96.27 | 5452 |
| IG01 | `CPU-0_SingleCpuUtil` | 1.27 | 100.0 | 102 |
| Redis02 | `MEMORY_NoCacheMemPerc` | 8.32 | 92.68 | 36 |
| Mysql02 | `LOCALDISK-sdc_DSKRead` | 0.019 | 7.33 | 122 |
| Redis02 | `MEMORY_MEMUsedMemPerc` | 17.2 | 98.0 | 13 |

### 02/03 — timing of each candidate (`02_timelines.py`, `03_raw_series.py`)

Sampling is ~1 point per KPI per 2 min.

- **Mysql01 sdb_DSKRead** — a *single* sample at 18:01 (96.27); 0.0 everywhere else.
- **IG01 CPU-0** — `SingleCpuidle` 98.7 (17:57) → 17.9 (17:59) → 0.0 (18:01, 18:03,
  18:05) → 72.0 (18:07) → 99.0 (18:09). Onset ~17:59, i.e. *before* the window.
  IG01's aggregate `CPUCpuUtil` never budges from 25.7 throughout.
- **Redis02 memory** — 23% flat until 18:09, then 98% at 18:10/12/13/15, then a
  permanent step down to 11%. `UserMem` 1668 → 7054 → 824 MB.

### 04 — system-level impact (`04_app_and_cpu.py`)

`metric_app` `sr` is 100.0 for every service across 17:45–18:45; the only `mrt`
excursion is a one-minute spike at 18:00 (avg 1098 ms, max 2755 ms across 7
services). No app-level impact at 18:10–18:15. So `metric_app` does not
discriminate here.

### 05 — whole-day scan, the decisive test (`05_dayscan.py`)

Materialised the three candidate KPIs for all of 2021-03-04 into
`day_candidates.parquet` and asked whether each pattern is one-off or routine:

- **IG01 CPU-0 idle = 0 → 91 samples across ~20 separate episodes that day**
  (00:49, 03:08–03:14, 03:52–03:58, 04:05–04:13, 06:37–06:49, 07:00–07:04, 08:54,
  10:05–10:13, 12:27–12:43, 14:29–14:41, a long 15:30–16:33 stretch, 17:59–18:05,
  20:40–20:44, 21:12–21:16, 22:27–22:34, 22:50–23:00). **Routine — ruled out.**
- **Mysql01 sdb_DSKRead > 10 → 7 times that day** (00:00 = 84.0, 03:14, 04:00,
  09:54, 12:49, 16:01, 18:01 = 96.27). Recurring, single-sample. **Ruled out.**
- **Redis02 MEMUsedMemPerc — 23.0 for 733 consecutive samples covering 00:00–18:09,
  then 98.0 for 18:10–18:15, then 11–14% for the rest of the day.** The only other
  excursion all day is 88% at 05:33–05:36, well outside this window. One-off, with
  a permanent post-event state change. **This is the failure.**

### 06 — confirming the reason (`06_confirm_redis02.py`)

- `MEMFreeMem` 5507 MB → **122 MB** (18:10–18:13) → 6353 MB (18:15). Memory
  genuinely exhausted, then released — the release overshoots the old baseline,
  consistent with the process being restarted.
- `UserMem` 1668 → 7054 MB: ~5.4 GB claimed by user processes, not by cache
  (`CacheMem` actually *falls*, 908 → 396 MB, as the kernel evicts cache).
- **Redis02 reports zero JVM/Heap KPIs**, so "high JVM CPU load" and "JVM Out of
  Memory (OOM) Heap" are not applicable to this component.
- No `gc`/`error`/`catalina` log lines on Redis02 in the window. The only
  non-access-log lines system-wide in 18:00–18:30 are routine `gc` on the four
  Tomcats (8–9 lines each).
- Redis02's other window anomalies all fire at the same instant and are secondary:
  `CPUUserTime` 0.17 → 3.14, `CPUSysTime` 0.45 → 2.42, `CPUWio` 0.008 → 3.57 at
  18:10–18:11, and `sda_DSKBps` 0 → 29 at 18:15 (the release).

### Conclusion

Single fault: **Redis02**, **high memory usage**, onset between 18:09 and 18:10
(occurrence time not requested by this task).
