# task_005 (task_6)

## Instruction (verbatim)

A single failure occurred within the time range of March 6, 2021, from 00:30 to
01:00. The specific component affected by this failure and the reason behind it
are currently unknown. You are tasked with identifying the root cause component
and the root cause reason for this failure.

## Asked for

- root cause component
- root cause reason

(occurrence datetime not requested)

## Window

Stated: 2021-03-06 00:30:00 – 01:00:00 (UTC+8, `ts`).
Materialised as a view with margin: 2021-03-05 22:30:00 – 2021-03-06 03:00:00,
so the baseline is 2h before and 2h after the stated window.

## Notes

### 01 — metric_container z-scan (dead end, see 08)

The window-vs-baseline scan is dominated by one host, and it is not the answer:

| cmdb_id | kpi | b_avg | w_max | z_max |
|---|---|---:|---:|---:|
| Tomcat01 | `sdb_DSKRead` | 0.0075 | 26905.07 | 869190 |
| Tomcat01 | `sdb_DSKRTps` | 0.0009 | 18.93 | 4882 |
| Tomcat01 | `sdb_DSKReadWrite` | 94.2 | 27880 | 272 |

Filesystem `FSUsedSpace`/`FSAvailableSpace` are flat, memory and JVM are flat, so if
this were the fault the reason would be high disk I/O read usage. It isn't — see 08.

### 02 — the disk spike is one sample

Filtering Tomcat01 `sdb_DSKRead > 0.5` over the whole 4.5 h margin returns exactly
one row, `00:55:00`. Its neighbours at 00:51 and 00:57 read `0.0`. `CPUWio` rises to
2.18 at 00:55 and is back to 0.03 by 00:57. No sustained disk pressure.

### 03 — logs are clean

No `error` or `catalina` lines anywhere in 23:30–02:00. `gc` counts on all four
Tomcats are flat (1–2 per 10 min through the window). `Mysql01`/`Mysql02` `slow`
volume is decaying normally from the pre-midnight peak. Nothing localises.

### 04 — traces: the real signal

Span self-duration (`duration` − Σ children) per cmdb_id, 5-min buckets. Everything
is flat except IG01/IG02, whose p99 goes from `2.0` ms to `13569` ms in the 00:50 and
00:55 buckets and back to `2.0` at 01:00. No network KPI moves anywhere (0 rows).

### 05 — the slow spans sit only on the IG tier

Spans over 5000 ms in 00:00–01:30, by host:

```
00:50  IG01 52   IG02 54
00:55  IG01 70   IG02 74
```

Nothing on MG, Tomcat or docker. Reading a slow trace tree: the IG root runs 13635 ms,
its Tomcat04 child starts **4.5 s later** and the whole Tomcat04 subtree finishes in
17 ms. The time is on the wire, not in any service.

### 06 — attribution: IG → Tomcat04, and only that edge

Mean gap between parent span start and child span start, per child host:

| bucket | →Tomcat01 | →Tomcat02 | →Tomcat03 | →Tomcat04 |
|---|---:|---:|---:|---:|
| 00:45 | 0.9 | 1.1 | 1.8 | 1.1 |
| 00:50 | 1.0 | 1.2 | 0.9 | **1063.2** |
| 00:55 | 2.4 | 1.1 | 1.4 | **2039.0** |
| 01:00 | 1.7 | 1.5 | 1.0 | 1.1 |

Across the whole system in 00:40–01:10, the *only* parent→child edges with a gap over
1000 ms are IG01→Tomcat04 (61) and IG02→Tomcat04 (64). Tomcat04's span counts are
unchanged, so it is serving normally — it is only reachable slowly.

Decomposing the 13.5 s IG span: 4.5 s gap + 0.07 s of Tomcat04 work + 8.9 s tail ≈
three ~4.5 s legs, i.e. a fixed one-way delay applied to Tomcat04's interface.

### 07 — latency, not packet loss

Per minute, IG → Tomcat04:

| minute | n | delayed | % | min gap | median | max gap |
|---|---:|---:|---:|---:|---:|---:|
| 00:52 | 43 | 0 | 0.0 | 1 | 1 | 2 |
| 00:53 | 29 | 28 | 96.6 | 1 | 4512 | 4595 |
| 00:54 | 25 | 25 | 100.0 | 4411 | 4484 | 4600 |
| 00:55 | 23 | 23 | 100.0 | 4413 | 4474 | 4589 |
| 00:56 | 22 | 22 | 100.0 | 4411 | 4515 | 4601 |
| 00:57 | 27 | 27 | 100.0 | 4405 | 4474 | 7126 |
| 00:58 | 47 | 0 | 0.0 | 1 | 1 | 2 |

**Every** request is delayed, by a tight unimodal 4404–4601 ms. Packet loss would
leave most requests untouched and pile the rest at discrete RTO steps (~1 s, 3 s,
7 s); the histogram has nothing between 104 ms and 4404 ms. This is a fixed delay.

Onset ≈ 00:53 (first minute affected, 96.6% — started mid-minute), clear by 00:58.

### 08 — ruling out the Tomcat01 disk spike

Multi-host `DSKRead` bursts over 500 on 2021-03-06, excluding apache01 (which reads
continuously all day):

```
2021-03-06 00:55:00   [Tomcat01, Tomcat03]                    max 26905
2021-03-06 23:55:00   [Mysql02, Mysql02, Mysql01, Tomcat02]   max 23109
```

Two bursts, both on the `:55` minute, same magnitude, different hosts — a scheduled
sweep, not a component fault. Every Tomcat `sdb_DSKRead` sample over 100 all day lands
at minute 55 (3 of 6). And it hits three hosts at once, which no single-component
fault does.

Tomcat04 itself has essentially no metric signature in 00:52–00:58: one weak
`CPU-2_SingleCpuUtil` at z=6.3. That is exactly what the brief says the two network
reasons look like — they are visible only in parent/child span latency.

## Answer

- **root cause component:** Tomcat04
- **root cause reason:** network latency

(Onset ≈ 2021-03-06 00:53:00, not requested.)
