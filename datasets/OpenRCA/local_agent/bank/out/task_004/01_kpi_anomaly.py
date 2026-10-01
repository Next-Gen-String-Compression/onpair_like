import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect(TASK / "task.duckdb")
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

P = "/Users/paul/workspace/onpair_like/datasets/OpenRCA/data/bank/parquet"

# Views only -- no extracts. Margin window 18:00-21:00 around the stated 19:00-19:30.
con.execute(f"""
CREATE OR REPLACE VIEW mc AS
SELECT * FROM '{P}/metric_container.parquet'
WHERE day = DATE '2021-03-04'
  AND ts BETWEEN TIMESTAMP '2021-03-04 18:00:00' AND TIMESTAMP '2021-03-04 21:00:00'
""")

con.execute(f"""
CREATE OR REPLACE VIEW mc_day AS
SELECT * FROM '{P}/metric_container.parquet'
WHERE day = DATE '2021-03-04'
""")

con.execute(f"""
CREATE OR REPLACE VIEW ma AS
SELECT * FROM '{P}/metric_app.parquet'
WHERE day = DATE '2021-03-04'
  AND ts BETWEEN TIMESTAMP '2021-03-04 18:00:00' AND TIMESTAMP '2021-03-04 21:00:00'
""")

con.execute(f"""
CREATE OR REPLACE VIEW lg AS
SELECT * FROM '{P}/log_service.parquet'
WHERE day = DATE '2021-03-04'
  AND ts BETWEEN TIMESTAMP '2021-03-04 18:00:00' AND TIMESTAMP '2021-03-04 21:00:00'
""")

print("=== coverage of the margin window ===")
con.sql("""
    SELECT count(*) AS rows, count(DISTINCT cmdb_id) AS hosts,
           count(DISTINCT kpi_name) AS kpis, min(ts) AS t0, max(ts) AS t1
    FROM mc
""").show(max_width=200)

# Window 19:00-19:35 (a little past the stated end, the recovery matters) vs the
# rest of the margin as baseline. Exclude 18:00-18:30: task_002's Redis02 fault.
print("\n=== |z| ranking, window 19:00-19:35 vs 18:30-21:00 baseline ===")
con.sql("""
    WITH tagged AS (
        SELECT cmdb_id, kpi_name, value, ts,
               ts BETWEEN TIMESTAMP '2021-03-04 19:00:00' AND TIMESTAMP '2021-03-04 19:35:00' AS in_win,
               ts <  TIMESTAMP '2021-03-04 18:30:00' AS pre
        FROM mc
    ),
    base AS (SELECT cmdb_id, kpi_name, avg(value) b_avg, stddev_samp(value) b_sd, count(*) b_n
             FROM tagged WHERE NOT in_win AND NOT pre GROUP BY 1,2),
    win  AS (SELECT cmdb_id, kpi_name, max(value) w_max, min(value) w_min, avg(value) w_avg, count(*) w_n
             FROM tagged WHERE in_win GROUP BY 1,2)
    SELECT cmdb_id, kpi_name, round(b_avg,3) AS b_avg, round(b_sd,3) AS b_sd,
           round(w_min,3) AS w_min, round(w_max,3) AS w_max,
           round((w_max - b_avg)/nullif(b_sd,0),1) AS z_max,
           round((w_min - b_avg)/nullif(b_sd,0),1) AS z_min
    FROM win JOIN base USING (cmdb_id, kpi_name)
    WHERE b_n >= 20 AND w_n >= 3
      AND greatest(abs((w_max - b_avg)/nullif(b_sd,0)), abs((w_min - b_avg)/nullif(b_sd,0))) > 6
    ORDER BY greatest(abs(z_max), abs(z_min)) DESC LIMIT 30
""").show(max_width=220, max_rows=40)

print("\n=== app KPIs, 5-min buckets over the margin ===")
con.sql("""
    SELECT time_bucket(INTERVAL '5 minutes', ts) AS b,
           round(min(sr),4) AS sr_min, round(max(mrt),1) AS mrt_max,
           round(avg(mrt),1) AS mrt_avg, round(max(rr),4) AS rr_max
    FROM ma GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=60)
