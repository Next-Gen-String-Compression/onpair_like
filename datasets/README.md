# Datasets

Pinned sources live in [`sources.yaml`](sources.yaml); `prepare.py` materialises
them (download → sha256 verify → extract column → parquet → `bench ingest`).

This file records dataset candidates that were **evaluated and rejected**, so
the same ground isn't covered twice.

## Partially accepted: SQLStorm v1.0 (2026-08-07)

Added as [`datasets/SQLStorm`](SQLStorm/README.md) — submodule + `prepare.py`.
Its 72 MB of LLM-generated SQL is kept as a corpus (mean row 1.16 kB, the only
long-string dataset here). Its **LIKE literals are not usable as a suite**:
58 of 64 distinct `p_name` needles match zero rows on `tpch-pname-sf10`, and the
six that do all sit at ~5.4% selectivity. Don't re-evaluate them; the numbers
are in that README. The `Tags LIKE CONCAT('%', TagName, '%')` join form (1,365
predicates) is the part still worth something, and needs a needle-set matcher
that doesn't exist yet.

The companion GitHub crawl (`like_predicates.csv`, repo root) **is** usable
where its predicates hit real columns: `suites/*-sqlstorm` (IMDB
keyword/aka_name + TPC-H p_name) via `SQLStorm/make_like_suites.py`; the IMDB
columns are pinned in `sources.yaml` as `imdb-keyword` / `imdb-akaname`.

## Rejected: Grafana o11y-bench (2026-08-05)

**What it was.** [o11y-bench](https://github.com/grafana/o11y-bench) is Grafana's
benchmark for LLM agents on observability tasks. Leaderboard submissions are
uploaded to the Hugging Face dataset repo
[`grafanalabs/o11y-bench-leaderboard`](https://huggingface.co/datasets/grafanalabs/o11y-bench-leaderboard),
which accumulates full Harbor job directories — including each trial's
`agent/trajectory.json`, the complete tool-call trace.

**Why it looked promising.** Those trajectories contain every LogQL query the
agents issued through the Grafana MCP server, and LogQL line filters
(`|= "x"`, `!= "y"`) are literal CONTAINS / NOT CONTAINS predicates over a long
string column — i.e. agent-authored `contains` needles from a real system,
which no other public corpus offers.

**What was actually there** (19 submissions, 7,745 trajectories, 1.1 GB clone):

| | |
|---|---|
| Loki tool calls | 22,215 |
| LogQL queries | 22,209 (8,756 distinct) |
| line filters | 13,884 |
| distinct needles | 1,102 |
| by operator | `\|=` 11,334 · `\|~` 2,501 · `!=` 42 · `!~` 7 |
| needle length | median 7 B, p90 18 B, max 62 B |

**Why it was rejected — the haystack, not the needles.**

1. **The corpus is 2.7 MB.** o11y-bench ships no log data; its Docker stack
   synthesises telemetry at eval time (`docker/python/src/o11y_stack/generate_data.py`).
   That generator touches the network in four places, so stubbing them and
   capturing the Loki push yields the whole corpus offline in about a second:
   **11,262 lines, 2.66 MB, 237 B/line**. That is the entire haystack any agent
   ever sees. The columns in `sources.yaml` are 36–240 MB.
2. **There are 35 distinct message strings.** `request completed` is 10,648 of
   the 11,262 lines. Asking for more simulated hours yields more near-duplicates,
   which would flatter every compressor rather than fix the size problem.
3. **Harvesting the trajectories instead doesn't help.** The log lines Loki
   returned are recoverable from the observations, giving 20,244 distinct lines
   (after discarding `line_format` output, which is rendered query results and
   not corpus). But those come from ~9 separate stack instances with different
   data end times, so the extra rows are timestamp and trace-ID churn: 9 runs'
   worth of rows carry 151 distinct `(service, message)` pairs against a single
   run's 106. It is also a filtered sample — a line is present only because it
   matched some agent's filter — which inflates measured selectivity ~2.4×
   (median 2.10% harvested vs 0.88% on the full generated corpus).
4. **The needles don't transfer.** Pairing them with a real log column was the
   obvious fallback, but they are microservice-specific (`cache`, `deploy`,
   `rollout`, `user-service`) while loghub HDFS is block-replication logs: 5 of
   the top 60 needles occur in it, two of which are `5` and `500`. A suite built
   that way is nearly all zero-match. Suites are bound to one dataset by
   checksum anyway, so cross-domain needle reuse was never the right shape.

**Incidental findings, if this is ever revisited.** Exclude filters are rare in
practice — 49 of 13,884 (0.4%) — so the "errors but not healthchecks" pattern
that motivates NOT CONTAINS is not what agents actually write; they write
single-needle `|=` chains (10,068 of 11,801 filtered queries have chain length 1).
Note also that a LogQL `|=` chain is an unordered conjunction, which is *not*
`LB_MULTI_CONTAINS` (ordered, non-overlapping, `LIKE '%a%b%'`); folding one into
the other would be wrong.
