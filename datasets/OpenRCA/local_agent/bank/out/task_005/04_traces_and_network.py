import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect(TASK / "task.duckdb")
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

PQ = "/Users/paul/workspace/onpair_like/datasets/OpenRCA/data/bank/parquet"

con.execute(f"""
CREATE OR REPLACE VIEW ts_win AS
SELECT cmdb_id, parent_id, span_id, trace_id, duration, ts
FROM '{PQ}/trace_span.parquet'
WHERE ts BETWEEN TIMESTAMP '2021-03-06 00:00:00' AND TIMESTAMP '2021-03-06 01:30:00'
  AND day = DATE '2021-03-06';
""")

print("=== span self-duration (duration - sum(children)) per cmdb_id, 10-min buckets ===")
con.sql("""
    WITH child_sum AS (
        SELECT parent_id, sum(duration) AS kids
        FROM ts_win WHERE parent_id IS NOT NULL AND parent_id <> ''
        GROUP BY 1
    ),
    self AS (
        SELECT s.cmdb_id, s.ts,
               s.duration - coalesce(c.kids, 0) AS self_dur
        FROM ts_win s LEFT JOIN child_sum c ON c.parent_id = s.span_id
    )
    SELECT cmdb_id,
           round(avg(self_dur) FILTER (WHERE ts < TIMESTAMP '2021-03-06 00:30:00'),1) AS pre_avg,
           round(avg(self_dur) FILTER (WHERE ts BETWEEN TIMESTAMP '2021-03-06 00:30:00'
                                                    AND TIMESTAMP '2021-03-06 01:00:00'),1) AS win_avg,
           round(avg(self_dur) FILTER (WHERE ts > TIMESTAMP '2021-03-06 01:00:00'),1) AS post_avg,
           round(quantile_cont(self_dur, 0.99) FILTER (WHERE ts BETWEEN TIMESTAMP '2021-03-06 00:30:00'
                                                    AND TIMESTAMP '2021-03-06 01:00:00'),1) AS win_p99,
           round(quantile_cont(self_dur, 0.99) FILTER (WHERE ts < TIMESTAMP '2021-03-06 00:30:00'),1) AS pre_p99,
           count(*) AS n
    FROM self GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=40)

print("\n=== self-duration per cmdb_id, 5-min buckets, avg + p99 ===")
con.sql("""
    WITH child_sum AS (
        SELECT parent_id, sum(duration) AS kids
        FROM ts_win WHERE parent_id IS NOT NULL AND parent_id <> ''
        GROUP BY 1
    ),
    self AS (
        SELECT s.cmdb_id, s.ts, s.duration - coalesce(c.kids, 0) AS self_dur
        FROM ts_win s LEFT JOIN child_sum c ON c.parent_id = s.span_id
    )
    SELECT time_bucket(INTERVAL '5 minutes', ts) AS b, cmdb_id,
           round(avg(self_dur),1) AS avg_self,
           round(quantile_cont(self_dur, 0.99),1) AS p99_self,
           count(*) AS n
    FROM self GROUP BY 1,2
    HAVING count(*) > 50
    ORDER BY 1,2
""").show(max_width=200, max_rows=300)

print("\n=== network KPIs: window vs baseline, any host ===")
con.sql("""
    WITH t AS (
        SELECT cmdb_id, kpi_name, value,
               ts BETWEEN TIMESTAMP '2021-03-06 00:30:00' AND TIMESTAMP '2021-03-06 01:00:00' AS in_win
        FROM mc
        WHERE kpi_name ILIKE '%NETWORK%' OR kpi_name ILIKE '%TCP%'
           OR kpi_name ILIKE '%Retrans%' OR kpi_name ILIKE '%Drop%' OR kpi_name ILIKE '%Err%'
    )
    SELECT cmdb_id, kpi_name,
           round(avg(value) FILTER (WHERE NOT in_win),3) AS b_avg,
           round(max(value) FILTER (WHERE NOT in_win),3) AS b_max,
           round(avg(value) FILTER (WHERE in_win),3)     AS w_avg,
           round(max(value) FILTER (WHERE in_win),3)     AS w_max
    FROM t GROUP BY 1,2
    HAVING max(value) FILTER (WHERE in_win) > 1.5 * nullif(max(value) FILTER (WHERE NOT in_win),0)
    ORDER BY 1,2
""").show(max_width=200, max_rows=60)

print("\n=== metric_app inside 00:20-01:10 only ===")
con.sql("""
    SELECT time_bucket(INTERVAL '5 minutes', ts) AS b,
           round(min(rr),2) AS rr_min, round(min(sr),2) AS sr_min,
           round(max(mrt),1) AS mrt_max, arg_max(tc, mrt) AS worst_tc
    FROM ma WHERE ts BETWEEN TIMESTAMP '2021-03-06 00:20:00' AND TIMESTAMP '2021-03-06 01:10:00'
    GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=40)
