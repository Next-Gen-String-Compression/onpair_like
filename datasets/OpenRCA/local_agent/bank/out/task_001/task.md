# task_001 (task_1)

## Instruction

> On March 4, 2021, within the time range of 14:30 to 15:00, a single failure was
> detected in the system. The exact time when the root cause occurred is unknown.
> Please identify the specific occurrence time of the root cause.

Asked for: **root cause occurrence datetime** only.

## Notes

**metric_container is clean.** Best z-score in 14:15–15:05 against a same-day
baseline is 24.7 on `Mysql02 Aborted Clients`, a counter whose whole range is
0–0.034. Nothing on CPU, memory, disk or filesystem leaves its baseline envelope.
Per the brief, that points at one of the two network reasons, which have no KPI
signature.

**metric_app** dips on `rr` at exactly two minutes: 14:33 (97.44) and 14:34
(91.49), plus 14:44 (90.48). `sr` stays 100 throughout.

**trace_span self-latency** is the signal. Self time = span duration minus the
summed durations of its direct children. MG01 and MG02 sit at max ~1000 ms per
minute all afternoon, then:

| minute | MG01 max self_ms | MG02 max self_ms |
|---|---:|---:|
| 14:32 | 1185 | 1045 |
| 14:33 | 6756 | 7552 |
| 14:34 | 7771 | 9277 |
| 14:35 | 2134 | 2318 |
| 14:36–14:42 | ~1030 | ~1030 |
| 14:43 | 5964 | 5879 |
| 14:44 | 11110 | 12368 |
| 14:45 | 1108 | 1076 |

At 10-second resolution the first affected bucket is **14:33:50**; nothing
exceeds 2000 ms between 14:25:00 and 14:33:50. The spans in that bucket run to
7.5 s, so they were issued around 14:33:42.

Two bursts ~10 min apart, both ~30 s wide, both on MG01 and MG02 together. The
same shape appears at 13:50–13:55 (outside this window), so it is the fault
signature rather than background noise. Reporting the onset, not the larger
14:44 peak, per the brief.

**Trap avoided:** a first pass slicing trace_span at exactly 14:30 showed slow
spans from 14:30:00. That was an edge artifact — `ts` is the span end time, so
parents at the left edge lose their children in the self-time join and report
`duration - 0`. The trace window needs margin either side; with a 14:10–15:10
slice the 14:30–14:33 hits disappear.

## Answer

`2021-03-04 14:33:00`
