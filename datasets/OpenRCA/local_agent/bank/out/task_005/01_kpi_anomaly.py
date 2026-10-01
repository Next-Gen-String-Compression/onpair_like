import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect(TASK / "task.duckdb")
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

PQ = "/Users/paul/workspace/onpair_like/datasets/OpenRCA/data/bank/parquet"

WIN_LO = "TIMESTAMP '2021-03-06 00:30:00'"
WIN_HI = "TIMESTAMP '2021-03-06 01:00:00'"

con.execute(f"""
CREATE OR REPLACE VIEW mc AS
SELECT * FROM '{PQ}/metric_container.parquet'
WHERE ts BETWEEN TIMESTAMP '2021-03-05 22:30:00' AND TIMESTAMP '2021-03-06 03:00:00'
  AND day IN (DATE '2021-03-05', DATE '2021-03-06');
""")

con.execute(f"""
CREATE OR REPLACE VIEW ma AS
SELECT * FROM '{PQ}/metric_app.parquet'
WHERE ts BETWEEN TIMESTAMP '2021-03-05 22:30:00' AND TIMESTAMP '2021-03-06 03:00:00'
  AND day IN (DATE '2021-03-05', DATE '2021-03-06');
""")

print("=== coverage: rows per cmdb_id ===")
con.sql("""
    SELECT cmdb_id, count(*) AS n, count(DISTINCT kpi_name) AS kpis,
           min(ts) AS t0, max(ts) AS t1
    FROM mc GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=60)

print("\n=== per (cmdb_id, kpi) window vs baseline, ranked by |z_max| ===")
con.sql(f"""
    WITH tagged AS (
        SELECT cmdb_id, kpi_name, ts, value,
               ts BETWEEN {WIN_LO} AND {WIN_HI} AS in_win
        FROM mc
    ),
    base AS (
        SELECT cmdb_id, kpi_name, avg(value) AS b_avg, stddev_samp(value) AS b_sd,
               max(value) AS b_max, count(*) AS b_n
        FROM tagged WHERE NOT in_win GROUP BY 1,2
    ),
    win AS (
        SELECT cmdb_id, kpi_name, avg(value) AS w_avg, max(value) AS w_max,
               min(value) AS w_min, count(*) AS w_n
        FROM tagged WHERE in_win GROUP BY 1,2
    )
    SELECT w.cmdb_id, w.kpi_name,
           round(b.b_avg,4) AS b_avg, round(b.b_max,4) AS b_max,
           round(w.w_avg,4) AS w_avg, round(w.w_max,4) AS w_max,
           round((w.w_max - b.b_avg) / nullif(b.b_sd,0), 2) AS z_max,
           round((w.w_avg - b.b_avg) / nullif(b.b_sd,0), 2) AS z_avg
    FROM win w JOIN base b USING (cmdb_id, kpi_name)
    WHERE b.b_n >= 20 AND w.w_n >= 3
      AND abs((w.w_max - b.b_avg) / nullif(b.b_sd,0)) > 4
    ORDER BY abs(z_max) DESC
    LIMIT 40
""").show(max_width=200, max_rows=60)

print("\n=== anomaly count per cmdb_id (|z_max| > 4) ===")
con.sql(f"""
    WITH tagged AS (
        SELECT cmdb_id, kpi_name, value,
               ts BETWEEN {WIN_LO} AND {WIN_HI} AS in_win
        FROM mc
    ),
    base AS (SELECT cmdb_id, kpi_name, avg(value) b_avg, stddev_samp(value) b_sd, count(*) b_n
             FROM tagged WHERE NOT in_win GROUP BY 1,2),
    win  AS (SELECT cmdb_id, kpi_name, max(value) w_max, count(*) w_n
             FROM tagged WHERE in_win GROUP BY 1,2)
    SELECT cmdb_id, count(*) AS n_anom
    FROM win JOIN base USING (cmdb_id, kpi_name)
    WHERE b_n >= 20 AND w_n >= 3
      AND abs((w_max - b_avg) / nullif(b_sd,0)) > 4
    GROUP BY 1 ORDER BY 2 DESC
""").show(max_width=200, max_rows=60)

print("\n=== metric_app: 5-min buckets across the margin window ===")
con.sql("""
    SELECT time_bucket(INTERVAL '5 minutes', ts) AS b, tc,
           round(avg(rr),3) AS rr, round(avg(sr),3) AS sr,
           round(avg(cnt),1) AS cnt, round(avg(mrt),1) AS mrt
    FROM ma GROUP BY 1,2 ORDER BY 1,2
""").show(max_width=200, max_rows=200)
