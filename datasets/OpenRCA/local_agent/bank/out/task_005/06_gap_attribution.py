import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect(TASK / "task.duckdb")
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

# For every parent->child edge in the window, the "gap" is how long after the parent
# span started before the child span started. A network fault on the child's host
# shows up as a large gap on edges pointing INTO that host.
print("=== slow IG traces 00:45-01:05: which host does the delayed child sit on? ===")
con.sql("""
    WITH slow_ig AS (
        SELECT trace_id, span_id, ts AS p_ts, cmdb_id AS p_host, duration AS p_dur
        FROM ts_win
        WHERE duration > 5000 AND cmdb_id IN ('IG01','IG02')
          AND ts BETWEEN TIMESTAMP '2021-03-06 00:45:00' AND TIMESTAMP '2021-03-06 01:05:00'
    )
    SELECT p.p_host, c.cmdb_id AS child_host, count(*) AS n_edges,
           round(avg(date_diff('millisecond', p.p_ts, c.ts)),0) AS avg_gap_ms,
           round(max(date_diff('millisecond', p.p_ts, c.ts)),0) AS max_gap_ms,
           round(avg(c.duration),1) AS avg_child_dur
    FROM slow_ig p JOIN ts_win c ON c.parent_id = p.span_id AND c.cmdb_id NOT IN ('IG01','IG02')
    GROUP BY 1,2 ORDER BY 1,2
""").show(max_width=200, max_rows=40)

print("\n=== baseline: same edge shape for ALL IG spans, 5-min buckets, by child host ===")
con.sql("""
    WITH ig AS (
        SELECT trace_id, span_id, ts AS p_ts, cmdb_id AS p_host, duration AS p_dur
        FROM ts_win WHERE cmdb_id IN ('IG01','IG02')
    )
    SELECT time_bucket(INTERVAL '5 minutes', p.p_ts) AS b, c.cmdb_id AS child_host,
           count(*) AS n,
           round(avg(date_diff('millisecond', p.p_ts, c.ts)),1) AS avg_gap_ms,
           round(quantile_cont(date_diff('millisecond', p.p_ts, c.ts), 0.99),0) AS p99_gap_ms
    FROM ig p JOIN ts_win c ON c.parent_id = p.span_id AND c.cmdb_id NOT IN ('IG01','IG02')
    WHERE p.p_ts BETWEEN TIMESTAMP '2021-03-06 00:30:00' AND TIMESTAMP '2021-03-06 01:15:00'
    GROUP BY 1,2 ORDER BY 1,2
""").show(max_width=200, max_rows=100)

print("\n=== every parent->child edge with gap > 1000ms, 00:40-01:10, by (parent host -> child host) ===")
con.sql("""
    SELECT p.cmdb_id AS parent_host, c.cmdb_id AS child_host,
           count(*) AS n, round(avg(date_diff('millisecond', p.ts, c.ts)),0) AS avg_gap,
           round(max(date_diff('millisecond', p.ts, c.ts)),0) AS max_gap
    FROM ts_win p JOIN ts_win c ON c.parent_id = p.span_id
    WHERE p.ts BETWEEN TIMESTAMP '2021-03-06 00:40:00' AND TIMESTAMP '2021-03-06 01:10:00'
      AND date_diff('millisecond', p.ts, c.ts) > 1000
      AND p.cmdb_id <> c.cmdb_id
    GROUP BY 1,2 ORDER BY 3 DESC
""").show(max_width=200, max_rows=40)

print("\n=== IG root span: total dur vs (gap to child) vs (tail after child ends) ===")
con.sql("""
    WITH slow_ig AS (
        SELECT trace_id, span_id, ts AS p_ts, cmdb_id AS p_host, duration AS p_dur
        FROM ts_win
        WHERE duration > 5000 AND cmdb_id IN ('IG01','IG02')
          AND ts BETWEEN TIMESTAMP '2021-03-06 00:45:00' AND TIMESTAMP '2021-03-06 01:05:00'
    ),
    edge AS (
        SELECT p.p_host, p.p_dur,
               date_diff('millisecond', p.p_ts, c.ts) AS gap_ms,
               c.duration AS child_dur,
               p.p_dur - date_diff('millisecond', p.p_ts, c.ts) - c.duration AS tail_ms
        FROM slow_ig p JOIN ts_win c ON c.parent_id = p.span_id AND c.cmdb_id NOT IN ('IG01','IG02')
    )
    SELECT p_host, count(*) AS n,
           round(avg(p_dur),0) AS avg_total, round(avg(gap_ms),0) AS avg_gap,
           round(avg(child_dur),1) AS avg_child, round(avg(tail_ms),0) AS avg_tail
    FROM edge GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=20)

print("\n=== does the fault also touch traces that never reach an IG? span counts by host over time ===")
con.sql("""
    SELECT time_bucket(INTERVAL '5 minutes', ts) AS b, cmdb_id, count(*) AS n
    FROM ts_win
    WHERE ts BETWEEN TIMESTAMP '2021-03-06 00:40:00' AND TIMESTAMP '2021-03-06 01:10:00'
    GROUP BY 1,2 ORDER BY 1,2
""").show(max_width=200, max_rows=120)
