import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect(TASK / "task.duckdb")
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

print("=== Tomcat01 sdb disk KPIs, 1-min, non-zero rows only ===")
con.sql("""
    SELECT ts,
           round(max(CASE WHEN kpi_name LIKE '%sdb_DSKRead'      THEN value END),2) AS sdb_read,
           round(max(CASE WHEN kpi_name LIKE '%sdb_DSKRTps'      THEN value END),3) AS sdb_rtps,
           round(max(CASE WHEN kpi_name LIKE '%sdb_DSKWrite'     THEN value END),2) AS sdb_write,
           round(max(CASE WHEN kpi_name LIKE '%sdb_DSKReadWrite' THEN value END),2) AS sdb_rw,
           round(max(CASE WHEN kpi_name LIKE '%sdb_DSKTps'       THEN value END),2) AS sdb_tps,
           round(max(CASE WHEN kpi_name LIKE '%sdb_DSKPercentBusy' THEN value END),3) AS sdb_busy
    FROM mc WHERE cmdb_id = 'Tomcat01'
    GROUP BY 1
    HAVING max(CASE WHEN kpi_name LIKE '%sdb_DSKRead' THEN value END) > 0.5
    ORDER BY 1
""").show(max_width=200, max_rows=100)

print("\n=== Tomcat01 CPU + WIO, 1-min around the window ===")
con.sql("""
    SELECT ts,
           round(max(CASE WHEN kpi_name = 'OSLinux-CPU_CPU_CPUCpuUtil'  THEN value END),2) AS cpu,
           round(max(CASE WHEN kpi_name = 'OSLinux-CPU_CPU_CPUWio'      THEN value END),3) AS wio,
           round(max(CASE WHEN kpi_name = 'OSLinux-CPU_CPU_CPUUserTime' THEN value END),2) AS usr,
           round(max(CASE WHEN kpi_name = 'OSLinux-CPU_CPU_CPUSystemTime' THEN value END),2) AS sys
    FROM mc WHERE cmdb_id = 'Tomcat01'
      AND ts BETWEEN TIMESTAMP '2021-03-06 00:15:00' AND TIMESTAMP '2021-03-06 01:15:00'
    GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=100)

print("\n=== Tomcat01 memory / disk-SPACE KPIs: window vs baseline (rule out other reasons) ===")
con.sql("""
    WITH t AS (
        SELECT kpi_name, value,
               ts BETWEEN TIMESTAMP '2021-03-06 00:30:00' AND TIMESTAMP '2021-03-06 01:00:00' AS in_win
        FROM mc WHERE cmdb_id = 'Tomcat01'
          AND (kpi_name ILIKE '%MEM%' OR kpi_name ILIKE '%Percent%'
               OR kpi_name ILIKE '%Space%' OR kpi_name ILIKE '%Used%'
               OR kpi_name ILIKE '%Swap%' OR kpi_name ILIKE '%JVM%' OR kpi_name ILIKE '%Heap%')
    )
    SELECT kpi_name,
           round(avg(value) FILTER (WHERE NOT in_win),3) AS b_avg,
           round(max(value) FILTER (WHERE NOT in_win),3) AS b_max,
           round(avg(value) FILTER (WHERE in_win),3)     AS w_avg,
           round(max(value) FILTER (WHERE in_win),3)     AS w_max
    FROM t GROUP BY 1
    HAVING max(value) FILTER (WHERE in_win) IS NOT NULL
    ORDER BY 1
""").show(max_width=200, max_rows=80)

print("\n=== is the MG01/MG02 sda disk activity periodic? 5-min max across margin ===")
con.sql("""
    SELECT time_bucket(INTERVAL '5 minutes', ts) AS b,
           round(max(CASE WHEN cmdb_id='MG01' THEN value END),1) AS MG01,
           round(max(CASE WHEN cmdb_id='MG02' THEN value END),1) AS MG02,
           round(max(CASE WHEN cmdb_id='Tomcat01' THEN value END),1) AS Tomcat01_sda
    FROM mc
    WHERE kpi_name = 'OSLinux-OSLinux_LOCALDISK_LOCALDISK-sda_DSKRead'
    GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=80)

print("\n=== any other host with an sdb_DSKRead spike in the margin window? ===")
con.sql("""
    SELECT cmdb_id, round(max(value),1) AS mx,
           arg_max(ts, value) AS at_ts
    FROM mc WHERE kpi_name LIKE '%sdb_DSKRead'
    GROUP BY 1 ORDER BY 2 DESC
""").show(max_width=200, max_rows=40)
