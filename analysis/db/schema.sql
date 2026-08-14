-- DuckDB result store for the LIKE benchmark.
--
-- Derived, rebuildable index over results/<group>/<run>/results.jsonl, which
-- stays the source of truth (DESIGN.md §9). Five dimensions, one fact table,
-- one flat view. Drop the .duckdb file and reload at any time.
--
-- Surrogate ids keep joins narrow; the *_key columns beside them are readable
-- natural keys (not hashes) so re-ingest is idempotent and a broken join can be
-- diagnosed by eye. NULL is never part of a key: SQL treats NULLs as distinct,
-- which would silently duplicate every scanner-less system on re-ingest, so
-- keys coalesce absent parts to '-'.

-- ---------------------------------------------------------------- platform

CREATE SEQUENCE IF NOT EXISTS seq_platform START 1;

CREATE TABLE IF NOT EXISTS platform (
    platform_id   BIGINT PRIMARY KEY DEFAULT nextval('seq_platform'),
    -- hostname|os|arch|cpu_model|cores. Deliberately excludes rustc/harness
    -- version and cpu_governor (those live on run): a toolchain bump must not
    -- fork one physical machine into two platforms. hostname IS in the key, so
    -- two hosts with identical hardware are never merged -- DESIGN.md §9
    -- forbids merging rows from different machines.
    platform_key  VARCHAR NOT NULL UNIQUE,
    hostname      VARCHAR NOT NULL,
    os            VARCHAR NOT NULL,      -- 'macos 25.3.0', 'linux 7.0.0-1010-aws'
    arch          VARCHAR NOT NULL,      -- 'aarch64', 'x86_64'
    cpu_model     VARCHAR,               -- 'Apple M4 Pro', 'Intel(R) Xeon(R) 6975P-C'
    cpu_features  VARCHAR[],
    cores         INTEGER
);

-- --------------------------------------------------------------------- run

CREATE SEQUENCE IF NOT EXISTS seq_run START 1;

CREATE TABLE IF NOT EXISTS run (
    run_id             BIGINT PRIMARY KEY DEFAULT nextval('seq_run'),
    run_key            VARCHAR NOT NULL UNIQUE,   -- run_dir@started_at
    run_dir            VARCHAR NOT NULL,          -- 'results/paper/clickbench-url-1m-contains'
    run_name           VARCHAR NOT NULL,          -- last path component
    run_group          VARCHAR,                   -- 'paper'; '' for results/sqlstorm
    platform_id        BIGINT NOT NULL REFERENCES platform (platform_id),
    started_at         TIMESTAMP NOT NULL,
    finished_at        TIMESTAMP,
    duration_s         DOUBLE,
    git_commit         VARCHAR,                   -- null when not a git checkout
    git_dirty          BOOLEAN,
    -- toolchain + hygiene: per-run, not per-machine
    rustc_version      VARCHAR,
    harness_version    VARCHAR,
    cpu_governor       VARCHAR,                   -- null = unknown (macOS, and AWS without cpufreq)
    pinned_core        INTEGER,
    pinning_effective  BOOLEAN,                   -- false on macOS
    -- measurement budget from the spec's [measure] block. Low values against
    -- the 3/10/200 defaults are the noise caveat any comparison has to respect
    warmup             INTEGER,
    min_iters          INTEGER,
    min_millis         BIGINT,
    -- spec_hash hashes the spec FILE, so the same experiment run from a
    -- different path hashes differently (the mac and x86 contains runs do).
    -- spec_content_key hashes what was actually measured -- sorted
    -- candidate/config list, scanners, strategy allowlist, measure block,
    -- dataset checksums, suite ids -- and is the key that pairs one experiment
    -- across machines.
    spec_hash          VARCHAR,
    spec_content_key   VARCHAR,
    -- content hash of results.jsonl: a rerun overwrites the file in place, so
    -- a changed hash means "replace this run's facts", not "already loaded"
    results_hash       VARCHAR,
    ingested_at        TIMESTAMP,
    -- A rerun into the same output directory is a NEW run row (history survives
    -- the JSONL being overwritten), so a run_dir can hold several runs. This
    -- marks the newest per run_dir; `WHERE is_latest` is the everyday filter.
    is_latest          BOOLEAN NOT NULL DEFAULT TRUE
);

-- ----------------------------------------------------------------- dataset

CREATE SEQUENCE IF NOT EXISTS seq_dataset START 1;

CREATE TABLE IF NOT EXISTS dataset (
    dataset_id     BIGINT PRIMARY KEY DEFAULT nextval('seq_dataset'),
    dataset_key    VARCHAR NOT NULL UNIQUE,   -- the checksum: a dataset's identity
    name           VARCHAR NOT NULL,          -- 'clickbench-url-1m', 'imdb-keyword'
    checksum       VARCHAR NOT NULL,
    num_rows       BIGINT NOT NULL,
    payload_bytes  BIGINT NOT NULL,
    payload_mb     DOUBLE,                    -- payload_bytes / 1e6
    -- payload_bytes + 8*(num_rows+1): the canonical view size, which is what
    -- puts the uncompressed baseline at ratio 1.0 by construction (DESIGN.md §9)
    raw_bytes      BIGINT NOT NULL,
    -- from datasets/<id>/manifest.json; null when that file is absent (the
    -- dataset artifacts are gitignored, so a fresh checkout has none)
    min_len        BIGINT,
    max_len        BIGINT,
    mean_len       DOUBLE
);

-- ------------------------------------------------------------------ system

CREATE SEQUENCE IF NOT EXISTS seq_system START 1;

-- The compared unit: a candidate at a config, answering through one strategy,
-- optionally via a harness scanner.
CREATE TABLE IF NOT EXISTS system (
    system_id          BIGINT PRIMARY KEY DEFAULT nextval('seq_system'),
    system_key         VARCHAR NOT NULL UNIQUE,   -- candidate|version|config|strategy|scanner
    label              VARCHAR NOT NULL,          -- 'onpair/decode+memmem', 'fsst_like_tum/interp'
    candidate          VARCHAR NOT NULL,
    candidate_version  VARCHAR NOT NULL,          -- opaque; embeds a submodule commit ('0.1.0+e638d4c')
    config             VARCHAR NOT NULL,          -- the JSON string handed to build(), '{}' in every run so far
    strategy           VARCHAR NOT NULL,
    scanner            VARCHAR                    -- only for harness-composed direct/decode
);

-- ------------------------------------------------------------------- query

CREATE SEQUENCE IF NOT EXISTS seq_query START 1;

CREATE TABLE IF NOT EXISTS query (
    query_pk         BIGINT PRIMARY KEY DEFAULT nextval('seq_query'),
    query_key        VARCHAR NOT NULL UNIQUE,   -- suite|query_id
    -- query_id alone is not unique: the filtered contains-s42 suite reuses 142
    -- ids from gen1-s42, so the suite has to be part of the key.
    suite            VARCHAR NOT NULL,
    query_id         VARCHAR NOT NULL,
    dataset_id       BIGINT NOT NULL REFERENCES dataset (dataset_id),
    op               VARCHAR NOT NULL,          -- prefix|suffix|contains|multi_contains|contains_any
    -- plain text; multi-needle ops join with ' | '. `bench gen` draws non-UTF-8
    -- needles, which render \xa7 here and set needle_is_binary.
    needle           VARCHAR NOT NULL,
    needle_is_binary BOOLEAN NOT NULL DEFAULT FALSE,
    needle_len       INTEGER NOT NULL,          -- total bytes across needles
    num_needles      INTEGER NOT NULL,
    -- blessed truth from the oracle, the x-axis of the headline figure
    selectivity      DOUBLE NOT NULL,
    match_count      BIGINT NOT NULL
);

-- ------------------------------------------------------------------ result

-- One row per measured cell. Compression is copied in from the run's build row
-- for the same candidate x config x dataset x chunk size, so ratio and speed
-- sit in one row with no join. (Verified: every measured cell has exactly one
-- such build row, and no build row lacks measured cells.)
CREATE TABLE IF NOT EXISTS result (
    run_id                 BIGINT NOT NULL REFERENCES run (run_id),
    system_id              BIGINT NOT NULL REFERENCES system (system_id),
    dataset_id             BIGINT NOT NULL REFERENCES dataset (dataset_id),
    query_pk               BIGINT NOT NULL REFERENCES query (query_pk),
    -- build-time knob from the spec (chunk_rows = [0, 122880]; 0 = whole column
    -- as one chunk). It changes footprint AND latency, and a spec may measure
    -- both values in one run -- so it is part of the key, and filtering it to a
    -- single value is what keeps a median from spanning two configurations.
    chunk_rows             BIGINT NOT NULL,

    status                 VARCHAR NOT NULL,   -- ok|unsupported|gate_failed|error
    -- false rather than null for unsupported/failed cells, so WHERE NOT gate_ok
    -- finds them instead of dropping them
    gate_ok                BOOLEAN NOT NULL,
    error                  VARCHAR,

    -- speed. total_ns is the median full-column query time over `samples`
    -- iterations -- the headline number; min_ns is the noise floor.
    total_ns               BIGINT,
    min_ns                 BIGINT,
    p25_ns                 BIGINT,
    p75_ns                 BIGINT,
    p99_ns                 BIGINT,
    max_ns                 BIGINT,
    mean_ns                DOUBLE,
    stddev_ns              DOUBLE,
    samples                INTEGER,
    -- ns_per_value / ns_per_domain_value / dedup_factor are NOT stored: they
    -- are total_ns over a denominator this schema already holds, so the view
    -- derives them rather than duplicating a fact.
    -- over payload bytes, which is the harness's own denominator -- NOT
    -- dataset.raw_bytes, the compression_ratio denominator. The two differ by
    -- ~9% on clickbench; keeping the names distinct keeps them from being mixed.
    gbps                   DOUBLE,

    -- The evaluation domain (ABI v5): the column values this system actually
    -- evaluated. NULL = one per row, which is every system except the dict_*
    -- front-ends; those evaluate the predicate once per UNIQUE value and
    -- scatter to rows, so their prefilter counters and per-value cost are
    -- unique-domain. Structural counters, not measurements -- identical in
    -- every sample -- which is why the view divides total_ns by them.
    eval_domain            BIGINT,
    eval_domain_matches    BIGINT,

    -- compression, from the matching build row
    compressed_bytes       BIGINT,             -- footprint_total_bytes
    raw_bytes              BIGINT,
    compression_ratio      DOUBLE,             -- raw_bytes / compressed_bytes
    build_ns               BIGINT,
    -- named byte components, candidate-defined (offsets, payload, codes,
    -- symbol_table, dict_bytes, token_stream, prefilter, ...): 13 spellings
    -- across the roster, so the only column that stays JSON
    footprint_components   JSON,

    -- prefilter attribution (DESIGN.md §10), scored against eval_domain.
    -- The *_ns columns below are SINGLE-SHOT instrumented-mode timings while
    -- total_ns is a median over samples, so they must never be divided into
    -- total_ns. *_origin says who held the clock: 'harness' at a pipeline
    -- joint, or 'self_reported' by the module.
    prefilter_candidates   BIGINT,
    prune_rate             DOUBLE,
    false_positive_rate    DOUBLE,
    verify_ns_per_survivor DOUBLE,
    setup_ns               BIGINT,
    setup_ns_origin        VARCHAR,
    decode_ns              BIGINT,
    decode_ns_origin       VARCHAR,
    scan_ns                BIGINT,
    scan_ns_origin         VARCHAR,

    PRIMARY KEY (run_id, system_id, dataset_id, chunk_rows, query_pk)
);

-- ----------------------------------------------------------------- the view

-- The one thing to query. Everything else is a WHERE/GROUP BY on this.
-- It carries hostname and arch on every row because DESIGN.md §9 forbids
-- merging measurements from different machines into one comparison: an
-- aggregate that spans them should be visible in the output, not implicit.
CREATE OR REPLACE VIEW v AS
SELECT
    p.hostname,
    p.arch,
    p.cpu_model,
    r.run_name,
    r.run_group,
    r.is_latest,                 -- false = an older run into the same directory
    r.started_at,
    r.min_iters,                 -- low values = treat the medians with care
    r.pinning_effective,
    s.label,
    s.candidate,
    s.candidate_version,
    s.strategy,
    s.scanner,
    s.config,
    d.name                       AS dataset,
    d.num_rows,
    d.payload_mb,
    d.mean_len,
    q.suite,
    q.query_id,
    q.op,
    q.needle,
    q.needle_len,
    q.num_needles,
    q.selectivity,
    q.match_count,
    res.chunk_rows,
    res.status,
    res.gate_ok,
    res.total_ns,
    res.min_ns,
    res.p99_ns,
    res.mean_ns,
    res.stddev_ns,
    res.samples,
    -- The two per-value costs, and the factor between them. ns_per_value is
    -- the end-to-end number (a dictionary's dedup win lands here);
    -- ns_per_domain_value is the cost per value actually evaluated, so it
    -- strips the dedup out and compares engines. On every row:
    --     ns_per_value = ns_per_domain_value / dedup_factor
    res.total_ns / d.num_rows                                  AS ns_per_value,
    res.total_ns / coalesce(res.eval_domain, d.num_rows)        AS ns_per_domain_value,
    d.num_rows::DOUBLE / coalesce(res.eval_domain, d.num_rows)  AS dedup_factor,
    coalesce(res.eval_domain, d.num_rows)                       AS eval_domain,
    res.eval_domain_matches,
    -- This query's selectivity inside the dictionary, against q.selectivity
    -- in the row domain: the gap is how skewed the matching values are.
    -- NULL for the row-domain systems, where the two coincide by definition.
    res.eval_domain_matches / res.eval_domain::DOUBLE           AS selectivity_domain,
    res.gbps,
    res.compressed_bytes,
    res.raw_bytes,
    res.compression_ratio,
    res.build_ns,
    res.prune_rate,
    res.false_positive_rate,
    res.setup_ns,
    res.decode_ns,
    res.scan_ns,
    res.run_id,
    res.system_id,
    res.dataset_id,
    res.query_pk
FROM result AS res
JOIN run      AS r ON r.run_id = res.run_id
JOIN platform AS p ON p.platform_id = r.platform_id
JOIN system   AS s ON s.system_id = res.system_id
JOIN dataset  AS d ON d.dataset_id = res.dataset_id
JOIN query    AS q ON q.query_pk = res.query_pk;
