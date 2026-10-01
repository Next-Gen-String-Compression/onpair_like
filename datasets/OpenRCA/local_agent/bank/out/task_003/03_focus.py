import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect()
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

W = f"'{TASK / 'metric_window.parquet'}'"

print("=== Tomcat01 memory KPIs, minute level 06:10-06:40 ===")
con.sql(f"""
    SELECT ts,
           round(max(CASE WHEN kpi_name LIKE '%NoCacheMemPerc' THEN value END),2) AS nocache_pct,
           round(max(CASE WHEN kpi_name LIKE '%MEMUsedMemPerc' THEN value END),2) AS used_pct,
           round(max(CASE WHEN kpi_name LIKE '%MEMFreeMem' THEN value END),0) AS free_mb,
           round(max(CASE WHEN kpi_name LIKE '%CacheMem' THEN value END),0) AS cache_mb,
           round(max(CASE WHEN kpi_name LIKE '%UserMem' THEN value END),0) AS user_mb,
           round(max(CASE WHEN kpi_name LIKE '%JVMMemoryUsedPercent' THEN value END),1) AS jvm_pct
    FROM {W} WHERE cmdb_id='Tomcat01' AND ts BETWEEN TIMESTAMP '2021-03-06 06:10:00'
                                                 AND TIMESTAMP '2021-03-06 06:40:00'
    GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=60)

print("\n=== Tomcat01 UserMem hourly shape (is the rise a slow climb?) 05:00-07:00 ===")
con.sql(f"""
    SELECT time_bucket(INTERVAL '5 minutes', ts) AS b,
           round(max(value),0) AS user_mb_max, round(min(value),0) AS user_mb_min
    FROM {W} WHERE cmdb_id='Tomcat01' AND kpi_name LIKE '%MEMORY_UserMem'
      AND ts BETWEEN TIMESTAMP '2021-03-06 05:00:00' AND TIMESTAMP '2021-03-06 07:00:00'
    GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=40)

print("\n=== NoCacheMemPerc peak per host in window (who else?) ===")
con.sql(f"""
    SELECT cmdb_id,
           round(max(CASE WHEN ts BETWEEN TIMESTAMP '2021-03-06 06:00:00'
                                     AND TIMESTAMP '2021-03-06 06:35:00' THEN value END),2) AS win_max,
           round(avg(CASE WHEN ts < TIMESTAMP '2021-03-06 06:00:00' THEN value END),2) AS pre_avg
    FROM {W} WHERE kpi_name LIKE '%NoCacheMemPerc'
    GROUP BY 1 ORDER BY win_max - pre_avg DESC NULLS LAST
""").show(max_width=200, max_rows=40)

print("\n=== the multi-host 05:5x DSKWrite spike: exact ts and hosts ===")
con.sql(f"""
    SELECT ts, cmdb_id, round(value,1) AS dskwrite
    FROM {W}
    WHERE kpi_name = 'OSLinux-OSLinux_LOCALDISK_LOCALDISK-sda_DSKWrite' AND value > 100
    ORDER BY ts, cmdb_id
""").show(max_width=200, max_rows=40)
