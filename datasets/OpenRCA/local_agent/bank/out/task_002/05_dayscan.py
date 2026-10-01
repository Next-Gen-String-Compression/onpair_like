import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect()
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

MC = "'datasets/OpenRCA/data/bank/parquet/metric_container.parquet'"
DAY = f"'{TASK / 'day_candidates.parquet'}'"

con.execute(f"""
    COPY (
        SELECT cmdb_id, kpi_name, ts, value
        FROM {MC}
        WHERE day = DATE '2021-03-04'
          AND ts BETWEEN TIMESTAMP '2021-03-04 00:00:00' AND TIMESTAMP '2021-03-05 00:00:00'
          AND (
              (cmdb_id = 'IG01'    AND kpi_name = 'OSLinux-CPU_CPU-0_SingleCpuidle')
           OR (cmdb_id = 'Redis02' AND kpi_name = 'OSLinux-OSLinux_MEMORY_MEMORY_MEMUsedMemPerc')
           OR (cmdb_id = 'Mysql01' AND kpi_name = 'OSLinux-OSLinux_LOCALDISK_LOCALDISK-sdb_DSKRead')
          )
    ) TO {DAY} (OVERWRITE_OR_IGNORE)
""")

print("=== IG01 CPU-0 saturated episodes over the whole day (idle < 50) ===")
con.sql(f"""
    SELECT ts, round(value,3) AS idle
    FROM {DAY}
    WHERE cmdb_id = 'IG01' AND value < 50
    ORDER BY ts
""").show(max_width=120, max_rows=100)

print("\n=== Redis02 MEMUsedMemPerc distinct levels over the day ===")
con.sql(f"""
    SELECT value AS used_pct, count(*) AS n, min(ts) AS first_ts, max(ts) AS last_ts
    FROM {DAY} WHERE cmdb_id = 'Redis02'
    GROUP BY 1 ORDER BY 1
""").show(max_width=120, max_rows=60)

print("\n=== Redis02 high-memory episodes (used% > 60) ===")
con.sql(f"""
    SELECT ts, value AS used_pct
    FROM {DAY} WHERE cmdb_id = 'Redis02' AND value > 60 ORDER BY ts
""").show(max_width=120, max_rows=100)

print("\n=== Mysql01 sdb_DSKRead spikes over the day (> 10) ===")
con.sql(f"""
    SELECT ts, round(value,3) AS dskread
    FROM {DAY} WHERE cmdb_id = 'Mysql01' AND value > 10 ORDER BY ts
""").show(max_width=120, max_rows=100)
