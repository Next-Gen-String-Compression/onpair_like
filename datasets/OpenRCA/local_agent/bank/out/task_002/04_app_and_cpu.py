import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect()
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

W = f"'{TASK / 'metric_window.parquet'}'"
APP = "'datasets/OpenRCA/data/bank/parquet/metric_app.parquet'"

print("=== metric_app per minute, 17:45-18:45 (all services) ===")
con.sql(f"""
    SELECT date_trunc('minute', ts) AS minute,
           round(avg(sr),4) AS sr, round(min(sr),4) AS sr_min,
           round(avg(mrt),2) AS mrt, round(max(mrt),2) AS mrt_max,
           round(sum(cnt),0) AS cnt, round(sum(rr),2) AS rr
    FROM {APP}
    WHERE day = DATE '2021-03-04'
      AND ts BETWEEN TIMESTAMP '2021-03-04 17:45:00' AND TIMESTAMP '2021-03-04 18:45:00'
    GROUP BY 1 ORDER BY 1
""").show(max_width=140, max_rows=80)

print("\n=== metric_app: services with worst sr in 18:00-18:30 ===")
con.sql(f"""
    SELECT tc, date_trunc('minute', ts) AS minute, sr, mrt, cnt, rr
    FROM {APP}
    WHERE day = DATE '2021-03-04'
      AND ts BETWEEN TIMESTAMP '2021-03-04 17:55:00' AND TIMESTAMP '2021-03-04 18:35:00'
      AND (sr < 1.0 OR mrt > 3 * (SELECT avg(mrt) FROM {APP}
                                  WHERE day = DATE '2021-03-04'
                                    AND ts BETWEEN TIMESTAMP '2021-03-04 17:00:00'
                                                AND TIMESTAMP '2021-03-04 17:55:00'))
    ORDER BY minute, tc
""").show(max_width=140, max_rows=100)

print("\n=== IG01 all CPU KPIs, 17:55-18:15 ===")
con.sql(f"""
    SELECT kpi_name, ts, round(value,3) AS value
    FROM {W}
    WHERE cmdb_id = 'IG01' AND kpi_name LIKE '%CPU%'
      AND ts BETWEEN TIMESTAMP '2021-03-04 17:55:00' AND TIMESTAMP '2021-03-04 18:15:00'
    ORDER BY kpi_name, ts
""").show(max_width=140, max_rows=300)
