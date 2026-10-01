import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect(TASK / "task.duckdb")
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

print("=== spans with duration > 5000 in 00:00-01:30, by cmdb_id and minute ===")
con.sql("""
    SELECT time_bucket(INTERVAL '5 minutes', ts) AS b, cmdb_id,
           count(*) AS n_slow, round(max(duration),0) AS max_dur
    FROM ts_win WHERE duration > 5000
    GROUP BY 1,2 ORDER BY 1,2
""").show(max_width=200, max_rows=80)

print("\n=== the slow traces: full span tree for traces containing a >5000ms span at 00:50-01:00 ===")
con.sql("""
    WITH slow_traces AS (
        SELECT DISTINCT trace_id FROM ts_win
        WHERE duration > 5000
          AND ts BETWEEN TIMESTAMP '2021-03-06 00:50:00' AND TIMESTAMP '2021-03-06 01:00:00'
    )
    SELECT s.trace_id, s.ts, s.cmdb_id, s.span_id, s.parent_id, s.duration
    FROM ts_win s JOIN slow_traces t USING (trace_id)
    ORDER BY s.trace_id, s.duration DESC
""").show(max_width=220, max_rows=120)

print("\n=== per-cmdb_id count of spans > 1000ms, 5-min buckets ===")
con.sql("""
    SELECT time_bucket(INTERVAL '5 minutes', ts) AS b,
           count(*) FILTER (WHERE cmdb_id='IG01')     AS IG01,
           count(*) FILTER (WHERE cmdb_id='IG02')     AS IG02,
           count(*) FILTER (WHERE cmdb_id='MG01')     AS MG01,
           count(*) FILTER (WHERE cmdb_id='MG02')     AS MG02,
           count(*) FILTER (WHERE cmdb_id='Tomcat01') AS T01,
           count(*) FILTER (WHERE cmdb_id='Tomcat02') AS T02,
           count(*) FILTER (WHERE cmdb_id='Tomcat03') AS T03,
           count(*) FILTER (WHERE cmdb_id='Tomcat04') AS T04,
           count(*) FILTER (WHERE cmdb_id LIKE 'docker%') AS dockers
    FROM ts_win WHERE duration > 1000
    GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=40)
