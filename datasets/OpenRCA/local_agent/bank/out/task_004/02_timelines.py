import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect(TASK / "task.duckdb")
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

print("=== top of the |z| ranking (head, cut off in 01) ===")
con.sql("""
    WITH tagged AS (
        SELECT cmdb_id, kpi_name, value,
               ts BETWEEN TIMESTAMP '2021-03-04 19:00:00' AND TIMESTAMP '2021-03-04 19:35:00' AS in_win,
               ts <  TIMESTAMP '2021-03-04 18:30:00' AS pre
        FROM mc
    ),
    base AS (SELECT cmdb_id, kpi_name, avg(value) b_avg, stddev_samp(value) b_sd, count(*) b_n
             FROM tagged WHERE NOT in_win AND NOT pre GROUP BY 1,2),
    win  AS (SELECT cmdb_id, kpi_name, max(value) w_max, min(value) w_min, count(*) w_n
             FROM tagged WHERE in_win GROUP BY 1,2)
    SELECT cmdb_id, kpi_name, round(b_avg,3) b_avg, round(w_max,3) w_max,
           round((w_max - b_avg)/nullif(b_sd,0),1) z_max
    FROM win JOIN base USING (cmdb_id, kpi_name)
    WHERE b_n >= 20 AND w_n >= 3
    ORDER BY abs(z_max) DESC LIMIT 12
""").show(max_width=220, max_rows=20)

print("\n=== how many anomalous KPIs per host (|z| > 6) ===")
con.sql("""
    WITH tagged AS (
        SELECT cmdb_id, kpi_name, value,
               ts BETWEEN TIMESTAMP '2021-03-04 19:00:00' AND TIMESTAMP '2021-03-04 19:35:00' AS in_win,
               ts <  TIMESTAMP '2021-03-04 18:30:00' AS pre
        FROM mc
    ),
    base AS (SELECT cmdb_id, kpi_name, avg(value) b_avg, stddev_samp(value) b_sd, count(*) b_n
             FROM tagged WHERE NOT in_win AND NOT pre GROUP BY 1,2),
    win  AS (SELECT cmdb_id, kpi_name, max(value) w_max, min(value) w_min, count(*) w_n
             FROM tagged WHERE in_win GROUP BY 1,2)
    SELECT cmdb_id,
           count(*) FILTER (WHERE greatest(abs((w_max-b_avg)/nullif(b_sd,0)),
                                           abs((w_min-b_avg)/nullif(b_sd,0))) > 6) AS n_anom,
           count(*) AS n_kpi
    FROM win JOIN base USING (cmdb_id, kpi_name)
    WHERE b_n >= 20 AND w_n >= 3
    GROUP BY 1 ORDER BY n_anom DESC
""").show(max_width=200, max_rows=30)

print("\n=== apache01 CPU / disk raw series, 18:40-20:00 ===")
con.sql("""
    SELECT ts,
           round(max(value) FILTER (WHERE kpi_name LIKE '%CPU_CPUCpuUtil'),2)          AS cpu_util,
           round(max(value) FILTER (WHERE kpi_name LIKE '%CPU_CPUidleutil'),2)         AS cpu_idle,
           round(max(value) FILTER (WHERE kpi_name LIKE '%CPU_CPUWio'),2)              AS cpu_wio,
           round(max(value) FILTER (WHERE kpi_name LIKE '%CPU_CPULoad'),2)             AS load,
           round(max(value) FILTER (WHERE kpi_name LIKE '%sda_DSKRead'),1)             AS sda_read,
           round(max(value) FILTER (WHERE kpi_name LIKE '%sdb_DSKRead'),1)             AS sdb_read,
           round(max(value) FILTER (WHERE kpi_name LIKE '%sda_DSKPercentBusy'),1)      AS sda_busy
    FROM mc
    WHERE cmdb_id = 'apache01'
      AND ts BETWEEN TIMESTAMP '2021-03-04 18:40:00' AND TIMESTAMP '2021-03-04 20:00:00'
    GROUP BY ts HAVING count(*) > 0 ORDER BY ts
""").show(max_width=220, max_rows=100)
