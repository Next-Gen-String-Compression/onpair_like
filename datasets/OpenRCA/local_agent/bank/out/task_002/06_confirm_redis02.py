import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect()
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

W = f"'{TASK / 'metric_window.parquet'}'"
LOG = "'datasets/OpenRCA/data/bank/parquet/log_service.parquet'"

print("=== Redis02 memory KPIs, 18:04-18:22 ===")
con.sql(f"""
    SELECT kpi_name, ts, round(value,2) AS value
    FROM {W}
    WHERE cmdb_id = 'Redis02' AND kpi_name LIKE '%MEMORY%'
      AND ts BETWEEN TIMESTAMP '2021-03-04 18:04:00' AND TIMESTAMP '2021-03-04 18:22:00'
    ORDER BY kpi_name, ts
""").show(max_width=140, max_rows=200)

print("\n=== Does Redis02 report any JVM KPI at all? ===")
con.sql(f"""
    SELECT kpi_name, count(*) AS n FROM {W}
    WHERE cmdb_id = 'Redis02' AND (kpi_name LIKE '%JVM%' OR kpi_name LIKE '%Heap%')
    GROUP BY 1 ORDER BY 1
""").show(max_width=140, max_rows=40)

print("\n=== Redis02 non-access-log lines, 17:50-18:40 ===")
con.sql(f"""
    SELECT log_name, ts, value
    FROM {LOG}
    WHERE day = DATE '2021-03-04' AND cmdb_id = 'Redis02'
      AND log_name NOT IN ('localhost_access_log', 'apache_access_log')
      AND ts BETWEEN TIMESTAMP '2021-03-04 17:50:00' AND TIMESTAMP '2021-03-04 18:40:00'
    ORDER BY ts
""").show(max_width=160, max_rows=60)

print("\n=== All non-access-log lines system-wide, 18:00-18:30, by component ===")
con.sql(f"""
    SELECT cmdb_id, log_name, count(*) AS n
    FROM {LOG}
    WHERE day = DATE '2021-03-04'
      AND log_name NOT IN ('localhost_access_log', 'apache_access_log')
      AND ts BETWEEN TIMESTAMP '2021-03-04 18:00:00' AND TIMESTAMP '2021-03-04 18:30:00'
    GROUP BY 1,2 ORDER BY 3 DESC
""").show(max_width=140, max_rows=40)
