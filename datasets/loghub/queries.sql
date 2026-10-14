-- Exploration queries for datasets/loghub/hdfs-v1.parquet (loghub HDFS_v1,
-- built by prepare_hdfs.py). Run with: duckdb :memory: -box -f queries.sql
-- from the repo root. Every query is bounded (aggregate or LIMIT); each is
-- preceded by a one-line note saying what it answers.

-- Sandbox: only the dataset is reachable, no network, config locked.
SET allowed_paths=['/Users/paul/workspace/onpair_like/datasets/loghub/hdfs-v1.parquet'];
SET enable_external_access=false;
SET allow_persistent_secrets=false;
SET lock_configuration=true;

-- Schema: 5 columns — ts TIMESTAMP, pid INTEGER, level/component/message VARCHAR.
DESCRIBE FROM 'datasets/loghub/hdfs-v1.parquet';

-- Size and capture window: 11,175,629 rows over 1 day 14:41:10 (2008-11-09 → 11-11).
SELECT count() AS rows, min(ts) AS first_event, max(ts) AS last_event,
       age(max(ts), min(ts)) AS span
FROM 'datasets/loghub/hdfs-v1.parquet';

-- Per-column profile; note approx_unique is HyperLogLog and overshoots badly
-- here (11.84M distinct messages claimed for 11.18M rows, 10 components for 9).
SUMMARIZE FROM 'datasets/loghub/hdfs-v1.parquet';

-- Level x component: 14 pairs, INFO/dfs.FSNamesystem is the biggest at 33.1%.
SELECT level, component, count() AS rows,
       round(100.0 * count() / sum(count()) OVER (), 3) AS pct
FROM 'datasets/loghub/hdfs-v1.parquet'
GROUP BY ALL ORDER BY rows DESC;

-- Exact message cardinality and length: 10,335,015 distinct (92.48%), 36-267 bytes, avg 92.6.
SELECT count(DISTINCT message) AS exact_distinct_messages,
       count() AS rows,
       round(100.0 * count(DISTINCT message) / count(), 2) AS pct_distinct,
       min(length(message)) AS min_len, round(avg(length(message)), 1) AS avg_len,
       max(length(message)) AS max_len,
       sum(strlen(message)) AS total_bytes
FROM 'datasets/loghub/hdfs-v1.parquet';

-- Templates behind that variety: masking block ids / IPs / integers collapses
-- 10.3M distinct messages to just 86 distinct templates.
SELECT count(DISTINCT template) AS distinct_templates FROM (
  SELECT regexp_replace(
           regexp_replace(
             regexp_replace(message, 'blk_-?[0-9]+', '<BLK>', 'g'),
             '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+(:[0-9]+)?', '<IP>', 'g'),
           '[0-9]+', '<N>', 'g') AS template
  FROM 'datasets/loghub/hdfs-v1.parquet');

-- Identifier prevalence and cardinality: every row carries a blk_ id (575,061
-- distinct), 66% carry an IP (204 distinct), 3,722 mention an Exception.
SELECT count(*) FILTER (WHERE message LIKE '%blk_%') AS has_block_id,
       count(*) FILTER (WHERE regexp_matches(message, '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+')) AS has_ip,
       count(*) FILTER (WHERE message LIKE '%/mnt/hadoop%') AS has_hdfs_path,
       count(*) FILTER (WHERE message LIKE '%Exception%') AS has_exception,
       count(DISTINCT regexp_extract(message, 'blk_-?[0-9]+')) AS distinct_block_ids,
       count(DISTINCT regexp_extract(message, '([0-9]+\.[0-9]+\.[0-9]+\.[0-9]+)', 1)) AS distinct_ips
FROM 'datasets/loghub/hdfs-v1.parquet';

-- The dozen hottest templates with their level/component — the six biggest
-- cover ~85% of the log.
SELECT level, component, template, count() AS rows FROM (
  SELECT level, component, regexp_replace(
           regexp_replace(
             regexp_replace(message, 'blk_-?[0-9]+', '<BLK>', 'g'),
             '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+(:[0-9]+)?', '<IP>', 'g'),
           '[0-9]+', '<N>', 'g') AS template
  FROM 'datasets/loghub/hdfs-v1.parquet')
GROUP BY ALL ORDER BY rows DESC LIMIT 12;

-- Hourly volume, WARN count and distinct threads — bursty, with WARN spikes
-- (56,758 of 81,102 rows in the 2008-11-09 22:00 hour).
SELECT date_trunc('hour', ts) AS hour, count() AS rows,
       count(*) FILTER (WHERE level = 'WARN') AS warns,
       count(DISTINCT pid) AS threads
FROM 'datasets/loghub/hdfs-v1.parquet'
GROUP BY ALL ORDER BY hour LIMIT 8;

-- Sample of the exception text: all "Failed to transfer ... Connection reset by peer".
SELECT message FROM 'datasets/loghub/hdfs-v1.parquet'
WHERE level = 'WARN' AND message LIKE '%Exception%' LIMIT 3;
