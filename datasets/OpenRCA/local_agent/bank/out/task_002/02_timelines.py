import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect()
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

W = f"'{TASK / 'metric_window.parquet'}'"

CANDS = [
    ("Mysql01", "OSLinux-OSLinux_LOCALDISK_LOCALDISK-sdb_DSKRead"),
    ("IG01",    "OSLinux-CPU_CPU-0_SingleCpuUtil"),
    ("Redis02", "OSLinux-OSLinux_MEMORY_MEMORY_MEMUsedMemPerc"),
    ("Redis02", "OSLinux-OSLinux_MEMORY_MEMORY_NoCacheMemPerc"),
    ("Mysql02", "OSLinux-OSLinux_LOCALDISK_LOCALDISK-sdc_DSKRead"),
]

for cmdb, kpi in CANDS:
    print(f"\n=== {cmdb} / {kpi} — non-baseline minutes ===")
    con.sql(f"""
        SELECT date_trunc('minute', ts) AS minute,
               round(min(value),3) AS lo, round(avg(value),3) AS mean, round(max(value),3) AS hi
        FROM {W}
        WHERE cmdb_id = '{cmdb}' AND kpi_name = '{kpi}'
        GROUP BY 1
        HAVING max(value) > (SELECT 3 * avg(value) + 0.001
                             FROM {W}
                             WHERE cmdb_id = '{cmdb}' AND kpi_name = '{kpi}')
        ORDER BY 1
    """).show(max_width=160, max_rows=80)
