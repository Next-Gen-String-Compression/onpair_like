import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect()
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

DAY = '2021-03-04'
W0 = '2021-03-04 14:30:00'
W1 = '2021-03-04 15:00:00'
PQ = 'datasets/OpenRCA/data/bank/parquet'

# Views, not copies: the parquets are clustered by `day` with per-row-group
# min/max on `ts`, so these predicates prune to a handful of row groups.
# The `day` predicate spans +/-1 because files spill either side of midnight.
con.execute(f"""
CREATE VIEW win AS
SELECT ts, cmdb_id, kpi_name, value
FROM '{PQ}/metric_container.parquet'
WHERE day BETWEEN DATE '{DAY}' - 1 AND DATE '{DAY}' + 1
  AND ts BETWEEN TIMESTAMP '{W0}' - INTERVAL 15 MINUTE
              AND TIMESTAMP '{W1}' + INTERVAL 5 MINUTE
  AND kpi_name NOT LIKE 'Container-DOCKER%'
""")

con.execute(f"""
CREATE VIEW base AS
SELECT ts, cmdb_id, kpi_name, value
FROM '{PQ}/metric_container.parquet'
WHERE day BETWEEN DATE '{DAY}' - 1 AND DATE '{DAY}' + 1
  AND ts::DATE = DATE '{DAY}'
  AND (ts < TIMESTAMP '{W0}' - INTERVAL 30 MINUTE
       OR ts > TIMESTAMP '{W1}' + INTERVAL 30 MINUTE)
  AND kpi_name NOT LIKE 'Container-DOCKER%'
""")


def show(title, sql):
    print('===', title, '===')
    rel = con.sql(sql)
    cols = rel.columns
    rows = rel.fetchall()
    w = [max([len(str(c))] + [len(str(r[i])) for r in rows]) for i, c in enumerate(cols)]
    print('  '.join(str(c).ljust(w[i]) for i, c in enumerate(cols)))
    for r in rows:
        print('  '.join(str(v).ljust(w[i]) for i, v in enumerate(r)))
    print()


show('top deviating (cmdb_id, kpi) in window', """
WITH b AS (
  SELECT cmdb_id, kpi_name,
         median(value) med, stddev_pop(value) sd,
         min(value) bmin, max(value) bmax
  FROM base GROUP BY 1,2
),
z AS (
  SELECT w.cmdb_id, w.kpi_name, w.ts, w.value, b.med, b.sd, b.bmin, b.bmax,
         abs(w.value - b.med) / greatest(b.sd, 0.02 * abs(b.med), 1e-9) AS zs
  FROM win w JOIN b USING (cmdb_id, kpi_name)
)
SELECT cmdb_id, kpi_name,
       round(max(zs),1) maxz,
       round(any_value(med),3) med, round(any_value(sd),3) sd,
       round(min(value),3) wmin, round(max(value),3) wmax,
       round(any_value(bmin),3) bmin, round(any_value(bmax),3) bmax,
       arg_max(ts, zs) t_peak
FROM z GROUP BY 1,2
HAVING max(zs) > 4 AND (max(value) > any_value(bmax) OR min(value) < any_value(bmin))
ORDER BY maxz DESC LIMIT 40
""")

show('metric_app per minute, aggregated over services', f"""
SELECT ts,
       round(min(sr),2) min_sr, round(min(rr),2) min_rr,
       sum(cnt) cnt, round(max(mrt),1) max_mrt, round(avg(mrt),1) avg_mrt,
       arg_min(tc, sr) worst_svc
FROM '{PQ}/metric_app.parquet'
WHERE day BETWEEN DATE '{DAY}' - 1 AND DATE '{DAY}' + 1
  AND ts BETWEEN TIMESTAMP '{W0}' - INTERVAL 15 MINUTE
              AND TIMESTAMP '{W1}' + INTERVAL 10 MINUTE
GROUP BY 1 ORDER BY 1
""")

# Self time = span duration minus the durations of its direct children.
# Bank's trace graph is horizontal, so this is the only view of the two
# network reasons, which leave no KPI signature.
con.execute(f"""
CREATE VIEW span AS
SELECT ts, cmdb_id, span_id, parent_id, duration
FROM '{PQ}/trace_span.parquet'
WHERE day BETWEEN DATE '{DAY}' - 1 AND DATE '{DAY}' + 1
  AND ts BETWEEN TIMESTAMP '{W0}' - INTERVAL 40 MINUTE
              AND TIMESTAMP '{W1}' + INTERVAL 20 MINUTE
""")

con.execute("""
CREATE VIEW selft AS
SELECT p.ts, p.cmdb_id, p.duration - coalesce(c.kids, 0) AS self_ms
FROM span p
LEFT JOIN (SELECT parent_id, sum(duration) kids FROM span GROUP BY 1) c
       ON c.parent_id = p.span_id
""")

show('slow spans (self_ms > 1000) per component per minute', """
SELECT time_bucket(INTERVAL 1 MINUTE, ts) t,
       count(*) FILTER (cmdb_id='MG01') MG01,
       count(*) FILTER (cmdb_id='MG02') MG02,
       count(*) FILTER (cmdb_id='IG01') IG01,
       count(*) FILTER (cmdb_id='IG02') IG02,
       count(*) FILTER (cmdb_id='Tomcat01') T01,
       count(*) FILTER (cmdb_id='Tomcat02') T02,
       count(*) FILTER (cmdb_id='Tomcat03') T03,
       count(*) FILTER (cmdb_id='Tomcat04') T04,
       count(*) FILTER (cmdb_id LIKE 'docker%') dock
FROM selft WHERE self_ms > 1000 GROUP BY 1 ORDER BY 1
""")

show('onset at 10 s resolution, MG01+MG02 slow spans', f"""
SELECT time_bucket(INTERVAL 10 SECOND, ts) t, count(*) n_slow, max(self_ms) mx
FROM selft
WHERE self_ms > 2000 AND cmdb_id IN ('MG01','MG02')
  AND ts BETWEEN TIMESTAMP '{W0}' - INTERVAL 5 MINUTE AND TIMESTAMP '{W1}'
GROUP BY 1 ORDER BY 1
""")

show('max self_ms per component per minute', """
SELECT time_bucket(INTERVAL 1 MINUTE, ts) t,
       max(self_ms) FILTER (cmdb_id='MG01') MG01,
       max(self_ms) FILTER (cmdb_id='MG02') MG02,
       max(self_ms) FILTER (cmdb_id='Tomcat01') T01,
       max(self_ms) FILTER (cmdb_id='Tomcat02') T02,
       max(self_ms) FILTER (cmdb_id='Tomcat03') T03,
       max(self_ms) FILTER (cmdb_id='Tomcat04') T04
FROM selft GROUP BY 1 ORDER BY 1
""")

