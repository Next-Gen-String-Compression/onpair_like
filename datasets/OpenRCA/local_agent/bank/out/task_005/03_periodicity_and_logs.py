import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect(TASK / "task.duckdb")
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

PQ = "/Users/paul/workspace/onpair_like/datasets/OpenRCA/data/bank/parquet"

# Full-day view: is a 00:55 sdb_DSKRead spike unique, or does it recur?
con.execute(f"""
CREATE OR REPLACE VIEW mc_day AS
SELECT * FROM '{PQ}/metric_container.parquet'
WHERE ts BETWEEN TIMESTAMP '2021-03-06 00:00:00' AND TIMESTAMP '2021-03-07 00:00:00'
  AND day IN (DATE '2021-03-06', DATE '2021-03-07');
""")

print("=== all sdb_DSKRead samples > 100 over the whole of 2021-03-06 ===")
con.sql("""
    SELECT ts, cmdb_id, round(value,1) AS v
    FROM mc_day WHERE kpi_name LIKE '%sdb_DSKRead' AND value > 100
    ORDER BY ts
""").show(max_width=200, max_rows=100)

print("\n=== all sda_DSKRead samples > 100 over the whole of 2021-03-06 ===")
con.sql("""
    SELECT ts, cmdb_id, round(value,1) AS v
    FROM mc_day WHERE kpi_name LIKE '%sda_DSKRead' AND value > 100
    ORDER BY ts
""").show(max_width=200, max_rows=100)

print("\n=== Tomcat01 vs Tomcat03 sdb_DSKRead / DSKRTps / DSKReadWrite at 00:50-01:05 ===")
con.sql("""
    SELECT ts, cmdb_id,
           round(max(CASE WHEN kpi_name LIKE '%sdb_DSKRead' THEN value END),1) AS rd,
           round(max(CASE WHEN kpi_name LIKE '%sdb_DSKRTps' THEN value END),3) AS rtps,
           round(max(CASE WHEN kpi_name LIKE '%sdb_DSKWrite' THEN value END),1) AS wr,
           round(max(CASE WHEN kpi_name LIKE '%sdb_DSKReadWrite' THEN value END),1) AS rw,
           round(max(CASE WHEN kpi_name LIKE '%sdb_DSKPercentBusy' THEN value END),3) AS busy
    FROM mc_day
    WHERE cmdb_id IN ('Tomcat01','Tomcat03')
      AND ts BETWEEN TIMESTAMP '2021-03-06 00:50:00' AND TIMESTAMP '2021-03-06 01:05:00'
    GROUP BY 1,2 ORDER BY 1,2
""").show(max_width=200, max_rows=60)

# Logs in the window
con.execute(f"""
CREATE OR REPLACE VIEW ls AS
SELECT * FROM '{PQ}/log_service.parquet'
WHERE ts BETWEEN TIMESTAMP '2021-03-05 23:30:00' AND TIMESTAMP '2021-03-06 02:00:00'
  AND day IN (DATE '2021-03-05', DATE '2021-03-06');
""")

print("\n=== non-access log volume per (cmdb_id, log_name), 10-min buckets ===")
con.sql("""
    SELECT time_bucket(INTERVAL '10 minutes', ts) AS b, cmdb_id, log_name, count(*) AS n
    FROM ls
    WHERE log_name NOT IN ('localhost_access_log','apache_access_log')
    GROUP BY 1,2,3 ORDER BY 1,2,3
""").show(max_width=200, max_rows=200)

print("\n=== error / catalina / slow log lines in the window ===")
con.sql("""
    SELECT ts, cmdb_id, log_name, substr(value, 1, 140) AS v
    FROM ls WHERE log_name IN ('error','catalina','slow','localhost')
    ORDER BY ts
""").show(max_width=220, max_rows=120)
