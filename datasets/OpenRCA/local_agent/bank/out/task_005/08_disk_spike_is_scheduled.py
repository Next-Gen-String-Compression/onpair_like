import duckdb
from pathlib import Path

TASK = Path(__file__).resolve().parent
con = duckdb.connect(TASK / "task.duckdb")
con.execute(f"CALL enable_logging(storage_path = '{TASK / 'queries.csv'}')")

# The Tomcat01 sdb_DSKRead spike at 00:55 is the only competing hypothesis.
# If large reads land on the :55 minute across the day and across hosts, it is a
# scheduled job, not an injected single-component fault.
print("=== minute-of-hour of every sdb_DSKRead sample > 100 on 2021-03-06 (Tomcats only) ===")
con.sql("""
    SELECT extract('minute' FROM ts) AS minute_of_hour, count(*) AS n,
           count(DISTINCT cmdb_id) AS hosts, round(max(value),0) AS max_v
    FROM mc_day
    WHERE kpi_name LIKE '%sdb_DSKRead' AND value > 100 AND cmdb_id LIKE 'Tomcat%'
    GROUP BY 1 ORDER BY 2 DESC
""").show(max_width=200, max_rows=70)

print("\n=== same for sda_DSKRead, all hosts except apache01 (which reads constantly) ===")
con.sql("""
    SELECT extract('minute' FROM ts) AS minute_of_hour, count(*) AS n,
           count(DISTINCT cmdb_id) AS hosts, round(max(value),0) AS max_v
    FROM mc_day
    WHERE kpi_name LIKE '%sda_DSKRead' AND value > 100 AND cmdb_id <> 'apache01'
    GROUP BY 1 ORDER BY 2 DESC
""").show(max_width=200, max_rows=70)

print("\n=== the :55 read bursts across the day: which hosts fire together? ===")
con.sql("""
    SELECT ts, list(cmdb_id ORDER BY value DESC) AS hosts,
           round(max(value),0) AS max_v, count(*) AS n_hosts
    FROM mc_day
    WHERE kpi_name LIKE '%sd__DSKRead' AND value > 500 AND cmdb_id <> 'apache01'
    GROUP BY 1 HAVING count(*) >= 2 ORDER BY 1
""").show(max_width=200, max_rows=60)

print("\n=== Tomcat04 KPIs in 00:50-01:00 vs baseline: confirm NO metric signature ===")
con.sql("""
    WITH t AS (
        SELECT kpi_name, value,
               ts BETWEEN TIMESTAMP '2021-03-06 00:52:00' AND TIMESTAMP '2021-03-06 00:58:00' AS in_win
        FROM mc WHERE cmdb_id = 'Tomcat04'
    ),
    s AS (
        SELECT kpi_name,
               avg(value) FILTER (WHERE NOT in_win) AS b_avg,
               stddev_samp(value) FILTER (WHERE NOT in_win) AS b_sd,
               max(value) FILTER (WHERE in_win) AS w_max,
               count(*) FILTER (WHERE in_win) AS w_n
        FROM t GROUP BY 1
    )
    SELECT kpi_name, round(b_avg,3) AS b_avg, round(w_max,3) AS w_max,
           round((w_max - b_avg)/nullif(b_sd,0),2) AS z
    FROM s WHERE w_n >= 2 AND abs((w_max - b_avg)/nullif(b_sd,0)) > 4
    ORDER BY abs((w_max - b_avg)/nullif(b_sd,0)) DESC
""").show(max_width=200, max_rows=30)
