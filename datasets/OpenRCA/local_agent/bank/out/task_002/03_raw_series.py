import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect()
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

W = f"'{TASK / 'metric_window.parquet'}'"

# Full raw series 17:45-18:45 for each candidate KPI, side by side per component.
GROUPS = {
    "Mysql01 disk read": ("Mysql01", "OSLinux-OSLinux_LOCALDISK_LOCALDISK-sdb_DSKRead"),
    "Mysql01 disk rtps": ("Mysql01", "OSLinux-OSLinux_LOCALDISK_LOCALDISK-sdb_DSKRTps"),
    "IG01 cpu0":         ("IG01",    "OSLinux-CPU_CPU-0_SingleCpuUtil"),
    "IG01 cpu total":    ("IG01",    "OSLinux-CPU_CPU_CPUCpuUtil"),
    "Redis02 mem used%": ("Redis02", "OSLinux-OSLinux_MEMORY_MEMORY_MEMUsedMemPerc"),
    "Redis02 usermem":   ("Redis02", "OSLinux-OSLinux_MEMORY_MEMORY_UserMem"),
}

for label, (cmdb, kpi) in GROUPS.items():
    print(f"\n=== {label}: {cmdb} / {kpi} ===")
    con.sql(f"""
        SELECT ts, value
        FROM {W}
        WHERE cmdb_id = '{cmdb}' AND kpi_name = '{kpi}'
          AND ts BETWEEN TIMESTAMP '2021-03-04 17:45:00' AND TIMESTAMP '2021-03-04 18:45:00'
        ORDER BY ts
    """).show(max_width=120, max_rows=200)
