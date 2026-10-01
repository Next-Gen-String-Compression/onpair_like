import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect(TASK / "task.duckdb")
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

con.execute("""
CREATE OR REPLACE VIEW ig_t04 AS
SELECT p.cmdb_id AS ig, p.ts AS p_ts, p.duration AS p_dur,
       date_diff('millisecond', p.ts, c.ts) AS gap_ms, c.duration AS child_dur
FROM ts_win p JOIN ts_win c ON c.parent_id = p.span_id
WHERE p.cmdb_id IN ('IG01','IG02') AND c.cmdb_id = 'Tomcat04';
""")

print("=== per minute, IG -> Tomcat04: is EVERY request delayed, or only a fraction? ===")
con.sql("""
    SELECT date_trunc('minute', p_ts) AS m, count(*) AS n,
           count(*) FILTER (WHERE gap_ms > 500) AS n_slow,
           round(100.0 * count(*) FILTER (WHERE gap_ms > 500) / count(*), 1) AS pct_slow,
           round(min(gap_ms),0) AS min_gap,
           round(median(gap_ms),0) AS med_gap,
           round(avg(gap_ms),0) AS avg_gap,
           round(max(gap_ms),0) AS max_gap
    FROM ig_t04
    WHERE p_ts BETWEEN TIMESTAMP '2021-03-06 00:44:00' AND TIMESTAMP '2021-03-06 01:06:00'
    GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=40)

print("\n=== histogram of gap_ms for IG -> Tomcat04, 00:50-01:00 ===")
con.sql("""
    SELECT CASE
             WHEN gap_ms < 10   THEN 'a: <10ms'
             WHEN gap_ms < 500  THEN 'b: 10-500ms'
             WHEN gap_ms < 1500 THEN 'c: 0.5-1.5s'
             WHEN gap_ms < 3000 THEN 'd: 1.5-3s'
             WHEN gap_ms < 5000 THEN 'e: 3-5s'
             ELSE                    'f: >5s'
           END AS bucket,
           count(*) AS n, round(min(gap_ms),0) AS mn, round(max(gap_ms),0) AS mx
    FROM ig_t04
    WHERE p_ts BETWEEN TIMESTAMP '2021-03-06 00:50:00' AND TIMESTAMP '2021-03-06 01:00:00'
    GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=20)

print("\n=== exact gap values of the delayed edges (are they clustered at RTO steps?) ===")
con.sql("""
    SELECT gap_ms, count(*) AS n
    FROM ig_t04
    WHERE p_ts BETWEEN TIMESTAMP '2021-03-06 00:50:00' AND TIMESTAMP '2021-03-06 01:00:00'
      AND gap_ms > 500
    GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=80)

print("\n=== same, for the 'tail' (parent duration minus gap minus child duration) ===")
con.sql("""
    SELECT CASE
             WHEN p_dur - gap_ms - child_dur < 500   THEN 'a: <0.5s'
             WHEN p_dur - gap_ms - child_dur < 3000  THEN 'b: 0.5-3s'
             WHEN p_dur - gap_ms - child_dur < 6000  THEN 'c: 3-6s'
             WHEN p_dur - gap_ms - child_dur < 10000 THEN 'd: 6-10s'
             ELSE                                         'e: >10s'
           END AS tail_bucket,
           count(*) AS n,
           round(min(p_dur - gap_ms - child_dur),0) AS mn,
           round(max(p_dur - gap_ms - child_dur),0) AS mx
    FROM ig_t04
    WHERE p_ts BETWEEN TIMESTAMP '2021-03-06 00:50:00' AND TIMESTAMP '2021-03-06 01:00:00'
    GROUP BY 1 ORDER BY 1
""").show(max_width=200, max_rows=20)

print("\n=== control: IG -> Tomcat01/02/03 per minute over the same span ===")
con.sql("""
    SELECT date_trunc('minute', p.ts) AS m, c.cmdb_id,
           count(*) AS n,
           count(*) FILTER (WHERE date_diff('millisecond', p.ts, c.ts) > 500) AS n_slow,
           round(max(date_diff('millisecond', p.ts, c.ts)),0) AS max_gap
    FROM ts_win p JOIN ts_win c ON c.parent_id = p.span_id
    WHERE p.cmdb_id IN ('IG01','IG02') AND c.cmdb_id IN ('Tomcat01','Tomcat02','Tomcat03')
      AND p.ts BETWEEN TIMESTAMP '2021-03-06 00:50:00' AND TIMESTAMP '2021-03-06 01:00:00'
    GROUP BY 1,2 ORDER BY 1,2
""").show(max_width=200, max_rows=60)
