import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect()
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

W = f"'{TASK / 'metric_window.parquet'}'"
WIN_LO = "TIMESTAMP '2021-03-06 05:55:00'"
WIN_HI = "TIMESTAMP '2021-03-06 06:40:00'"

print("=== cmdb_ids present ===")
con.sql(f"""
    SELECT cmdb_id, count(*) AS n, count(DISTINCT kpi_name) AS kpis
    FROM {W} GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=60)

print("\n=== per (cmdb_id, kpi) window vs baseline, ranked by |z_max| ===")
con.sql(f"""
    WITH tagged AS (
        SELECT cmdb_id, kpi_name, ts, value,
               ts BETWEEN {WIN_LO} AND {WIN_HI} AS in_win
        FROM {W}
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
        FROM {W}
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
