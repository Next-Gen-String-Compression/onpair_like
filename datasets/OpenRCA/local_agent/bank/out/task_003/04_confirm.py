import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect()
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

W = f"'{TASK / 'metric_window.parquet'}'"
LOG = "'/Users/paul/workspace/onpair_like/datasets/OpenRCA/data/bank/parquet/log_service.parquet'"

# Re-rank with the 05:50-06:00 multi-host disk-write burst excluded from BOTH sides.
print("=== anomalies strictly inside 06:00-06:35, burst window excluded from baseline ===")
con.sql(f"""
    WITH tagged AS (
        SELECT cmdb_id, kpi_name, value,
               ts BETWEEN TIMESTAMP '2021-03-06 06:00:00' AND TIMESTAMP '2021-03-06 06:35:00' AS in_win,
               ts BETWEEN TIMESTAMP '2021-03-06 05:50:00' AND TIMESTAMP '2021-03-06 06:00:00' AS burst
        FROM {W}
    ),
    base AS (SELECT cmdb_id, kpi_name, avg(value) b_avg, stddev_samp(value) b_sd, count(*) b_n
             FROM tagged WHERE NOT in_win AND NOT burst GROUP BY 1,2),
    win  AS (SELECT cmdb_id, kpi_name, max(value) w_max, avg(value) w_avg, count(*) w_n
             FROM tagged WHERE in_win GROUP BY 1,2)
    SELECT cmdb_id, kpi_name, round(b_avg,3) b_avg, round(w_max,3) w_max,
           round((w_max - b_avg)/nullif(b_sd,0),1) AS z_max
    FROM win JOIN base USING (cmdb_id, kpi_name)
    WHERE b_n >= 20 AND w_n >= 3 AND abs((w_max - b_avg)/nullif(b_sd,0)) > 8
    ORDER BY abs(z_max) DESC LIMIT 25
""").show(max_width=200, max_rows=40)

# Non-access-log lines on the day, around the window, per component.
print("\n=== diagnostic log lines 06:00-06:40 (gc / catalina / error / slow) ===")
con.sql(f"""
    SELECT cmdb_id, log_name, count(*) AS n
    FROM {LOG}
    WHERE day = DATE '2021-03-06'
      AND ts BETWEEN TIMESTAMP '2021-03-06 06:00:00' AND TIMESTAMP '2021-03-06 06:40:00'
      AND log_name NOT IN ('localhost_access_log','apache_access_log')
    GROUP BY 1,2 ORDER BY n DESC LIMIT 30
""").show(max_width=200, max_rows=40)

print("\n=== same counts for the quiet hour 05:00-05:40, for contrast ===")
con.sql(f"""
    SELECT cmdb_id, log_name, count(*) AS n
    FROM {LOG}
    WHERE day = DATE '2021-03-06'
      AND ts BETWEEN TIMESTAMP '2021-03-06 05:00:00' AND TIMESTAMP '2021-03-06 05:40:00'
      AND log_name NOT IN ('localhost_access_log','apache_access_log')
    GROUP BY 1,2 ORDER BY n DESC LIMIT 30
""").show(max_width=200, max_rows=40)

print("\n=== app KPIs (metric_app) around the window ===")
con.sql(f"""
    SELECT time_bucket(INTERVAL '5 minutes', ts) AS b, tc,
           round(avg(sr),4) AS sr, round(avg(mrt),1) AS mrt, round(avg(rr),4) AS rr
    FROM '/Users/paul/workspace/onpair_like/datasets/OpenRCA/data/bank/parquet/metric_app.parquet'
    WHERE ts BETWEEN TIMESTAMP '2021-03-06 06:00:00' AND TIMESTAMP '2021-03-06 06:40:00'
    GROUP BY 1,2 HAVING avg(sr) < 1 OR avg(rr) > 0 ORDER BY 1,2 LIMIT 40
""").show(max_width=200, max_rows=50)
