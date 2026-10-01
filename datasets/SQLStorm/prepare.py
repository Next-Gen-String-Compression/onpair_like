#!/usr/bin/env python3
"""Materialise the SQLStorm v1.0 query sets as two parquet tables.

Source is the `vendor/SQLStorm` submodule — no download, no network. The
submodule already holds every query as a plain `.sql` file, so this script only
reshapes them:

  queries.parquet         one row per query file
    dataset   large_string  'stackoverflow' | 'job' | 'tpch' | 'tpcds'
    query_id  int32         the filename stem (1000.sql -> 1000)
    sql       large_string  file contents, verbatim

  like_predicates.parquet one row per LIKE/ILIKE predicate found in those files
    dataset   large_string
    query_id  int32
    operand   large_string  left-hand side as written ('p.Tags', 'kind')
    op        large_string  'LIKE' | 'ILIKE' | 'NOT LIKE' | 'NOT ILIKE'
    rhs_kind  large_string  'literal' | 'correlated'
    pattern   large_string  the literal, NULL when rhs_kind = 'correlated'
    shape     large_string  'contains' | 'prefix' | 'suffix' | 'match-all' | …

`sql` is the string-matching workload; `bench ingest` canonicalises one string
column, so:

  bench ingest --source datasets/SQLStorm/queries.parquet --format parquet \
    --column sql --id sqlstorm-v1-sql --out datasets/sqlstorm-v1-sql

`like_predicates` is not a corpus — it is the needle inventory, kept because
SQLStorm's whole reason to be here is that its LIKE patterns were written by an
LLM against a real schema. See README.md for why most of them match nothing.

Only v1.0 is read. v0.0 is the parameterised official TPC-H/TPC-DS/JOB queries
and contains no LIKE at all.

Idempotent: an existing output parquet is left alone. Delete it to rebuild.

Deps: pyarrow (see datasets/requirements.txt).
"""
from __future__ import annotations

import re
from pathlib import Path

HERE = Path(__file__).resolve().parent
QUERY_ROOT = HERE / "vendor" / "SQLStorm" / "v1.0"
DATASETS = ("stackoverflow", "job", "tpch", "tpcds")

# `p.Tags NOT ILIKE '%<sql>%'` — operand, optional NOT, operator, literal.
# The literal body allows doubled '' (SQL's escaped quote); nothing in v1.0 uses
# a LIKE ... ESCAPE clause, so a backslash carries no special meaning here.
LITERAL_RE = re.compile(
    r"([\w.]+)\s+(NOT\s+)?(I?LIKE)\s+'((?:[^']|'')*)'", re.IGNORECASE)

# The other RHS the generator produces: a needle built per row from a joined
# column, `p.Tags LIKE CONCAT('%', t.TagName, '%')` or the `||` spelling. This
# is a LIKE-join, not a constant predicate, so there is no pattern to record.
CORRELATED_RE = re.compile(
    r"([\w.]+)\s+(NOT\s+)?(I?LIKE)\s+(?:CONCAT\s*\(|'[^']*'\s*\|\|)",
    re.IGNORECASE)


def shape_of(pattern: str) -> str:
    """Classify a LIKE literal by where its wildcards sit."""
    if pattern == "%":
        return "match-all"
    if "_" in pattern:
        return "underscore"
    if "%" not in pattern:
        return "equality"
    inner = "%" in pattern.strip("%")
    if pattern.startswith("%") and pattern.endswith("%"):
        return "multi-contains" if inner else "contains"
    if pattern.endswith("%"):
        return "prefix-multi" if inner else "prefix"
    if pattern.startswith("%"):
        return "suffix-multi" if inner else "suffix"
    return "other"


def log(msg: str) -> None:
    print(msg, flush=True)


def human(n: float) -> str:
    for unit in ("B", "KiB", "MiB", "GiB"):
        if n < 1024 or unit == "GiB":
            return f"{n:.1f} {unit}" if unit != "B" else f"{int(n)} B"
        n /= 1024.0
    return f"{n} B"


def query_files(dataset: str) -> list[tuple[int, Path]]:
    """Every query of `dataset`, ordered by numeric id (1.sql before 10.sql)."""
    d = QUERY_ROOT / dataset / "queries"
    if not d.is_dir():
        raise SystemExit(
            f"{d} missing — the submodule is not checked out. Run:\n"
            f"  git submodule update --init datasets/SQLStorm/vendor/SQLStorm")
    return sorted(((int(p.stem), p) for p in d.glob("*.sql")), key=lambda t: t[0])


def predicates(dataset: str, qid: int, sql: str):
    """Yield one row per LIKE predicate in `sql`, literals then correlated."""
    for m in LITERAL_RE.finditer(sql):
        operand, negated, op, pattern = m.groups()
        pattern = pattern.replace("''", "'")
        yield (dataset, qid, operand,
               (("NOT " if negated else "") + op).upper(),
               "literal", pattern, shape_of(pattern))
    for m in CORRELATED_RE.finditer(sql):
        operand, negated, op = m.groups()
        yield (dataset, qid, operand,
               (("NOT " if negated else "") + op).upper(),
               "correlated", None, "correlated")


def main() -> None:
    import pyarrow as pa
    import pyarrow.parquet as pq

    queries_out = HERE / "queries.parquet"
    preds_out = HERE / "like_predicates.parquet"
    if queries_out.exists() and preds_out.exists():
        log(f"{queries_out.name} and {preds_out.name} present — nothing to do "
            f"(delete them to rebuild)")
        return

    q_dataset: list[str] = []
    q_id: list[int] = []
    q_sql: list[str] = []
    preds: list[tuple] = []
    sql_bytes = 0

    for dataset in DATASETS:
        files = query_files(dataset)
        before = len(preds)
        for qid, path in files:
            sql = path.read_text(encoding="utf-8", errors="replace")
            q_dataset.append(dataset)
            q_id.append(qid)
            q_sql.append(sql)
            sql_bytes += len(sql.encode("utf-8"))
            preds.extend(predicates(dataset, qid, sql))
        log(f"  {dataset:14s} {len(files):6d} queries, "
            f"{len(preds) - before:5d} LIKE predicates")

    queries_schema = pa.schema([
        ("dataset", pa.large_string()),
        ("query_id", pa.int32()),
        ("sql", pa.large_string()),
    ])
    pq.write_table(
        pa.table([pa.array(q_dataset, pa.large_string()),
                  pa.array(q_id, pa.int32()),
                  pa.array(q_sql, pa.large_string())], schema=queries_schema),
        str(queries_out), compression="zstd")
    log(f"  {len(q_sql)} queries -> {queries_out.name} "
        f"({human(queries_out.stat().st_size)} on disk); "
        f"sql payload {human(sql_bytes)}")

    preds_schema = pa.schema([
        ("dataset", pa.large_string()),
        ("query_id", pa.int32()),
        ("operand", pa.large_string()),
        ("op", pa.large_string()),
        ("rhs_kind", pa.large_string()),
        ("pattern", pa.large_string()),
        ("shape", pa.large_string()),
    ])
    cols = list(zip(*preds)) if preds else [()] * 7
    pq.write_table(
        pa.table([pa.array(cols[0], pa.large_string()),
                  pa.array(cols[1], pa.int32()),
                  pa.array(cols[2], pa.large_string()),
                  pa.array(cols[3], pa.large_string()),
                  pa.array(cols[4], pa.large_string()),
                  pa.array(cols[5], pa.large_string()),
                  pa.array(cols[6], pa.large_string())], schema=preds_schema),
        str(preds_out), compression="zstd")
    log(f"  {len(preds)} predicates -> {preds_out.name} "
        f"({human(preds_out.stat().st_size)} on disk)")

    log(f"  ingest with: bench ingest --source {queries_out} --format parquet "
        f"--column sql --id sqlstorm-v1-sql --out datasets/sqlstorm-v1-sql")


if __name__ == "__main__":
    main()
