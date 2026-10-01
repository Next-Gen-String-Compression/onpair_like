import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect()
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

W = f"'{TASK / 'metric_window.parquet'}'"

# Is the disk-write spike a periodic system-wide event, or unique to the window?
print("=== DSKWrite max per 10-min bucket, per host (is it periodic?) ===")
con.sql(f"""
    SELECT time_bucket(INTERVAL '10 minutes', ts) AS b,
           round(max(CASE WHEN cmdb_id='Mysql01' THEN value END),1) AS Mysql01,
           round(max(CASE WHEN cmdb_id='Mysql02' THEN value END),1) AS Mysql02,
           round(max(CASE WHEN cmdb_id='apache01' THEN value END),1) AS apache01,
           round(max(CASE WHEN cmdb_id='Tomcat01' THEN value END),1) AS Tomcat01,
           round(max(CASE WHEN cmdb_id='Tomcat02' THEN value END),1) AS Tomcat02
    FROM {W}
    WHERE kpi_name = 'OSLinux-OSLinux_LOCALDISK_LOCALDISK-sda_DSKWrite'
    GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=40)

print("\n=== Tomcat01 DSKRead / DSKRTps / NoCacheMemPerc, 5-min buckets ===")
con.sql(f"""
    SELECT time_bucket(INTERVAL '5 minutes', ts) AS b,
           round(max(CASE WHEN kpi_name LIKE '%DSKRead' THEN value END),2) AS dsk_read_max,
           round(max(CASE WHEN kpi_name LIKE '%DSKRTps' THEN value END),3) AS dsk_rtps_max,
           round(max(CASE WHEN kpi_name LIKE '%NoCacheMemPerc' THEN value END),2) AS nocache_max,
           round(max(CASE WHEN kpi_name = 'OSLinux-CPU_CPU_CPUCpuUtil' THEN value END),2) AS cpu_max,
           round(max(CASE WHEN kpi_name = 'OSLinux-CPU_CPU_CPUWio' THEN value END),3) AS wio_max
    FROM {W} WHERE cmdb_id = 'Tomcat01'
    GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=60)

print("\n=== which hosts show DSKRead > 50 at all, and when ===")
con.sql(f"""
    SELECT cmdb_id, ts, round(value,1) AS v
    FROM {W}
    WHERE kpi_name = 'OSLinux-OSLinux_LOCALDISK_LOCALDISK-sda_DSKRead' AND value > 50
    ORDER BY ts
""").show(max_width=200, max_rows=60)

print("\n=== memory KPIs on Tomcat01 across window ===")
con.sql(f"""
    SELECT kpi_name, round(min(value),2) mn, round(avg(value),2) av, round(max(value),2) mx
    FROM {W} WHERE cmdb_id='Tomcat01' AND kpi_name ILIKE '%MEM%'
    GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=40)
