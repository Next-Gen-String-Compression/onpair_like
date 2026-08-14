#!/usr/bin/env python3
"""Load benchmark runs into results/bench.duckdb.

    load.py results/paper/clickbench-url-1m-contains   # one run (what `bench run` calls)
    load.py --all                                      # every run under results/

results.jsonl stays the source of truth (DESIGN.md §9); this DB is a derived
index. Loading is idempotent: a run already present with the same results.jsonl
is a no-op, and a run whose results.jsonl changed (a rerun overwrites it in
place) has its facts replaced.
"""

import base64
import hashlib
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

import duckdb

ROOT = Path(__file__).resolve().parents[2]
DB_PATH = ROOT / "results" / "bench.duckdb"
SCHEMA_PATH = Path(__file__).with_name("schema.sql")

# Result rows carry `strategy`/`scanner` as absent-or-string. SQL treats NULLs as
# distinct, so a key containing one would duplicate every scanner-less system on
# re-ingest; absent parts collapse to this sentinel instead.
NONE = "-"


# ------------------------------------------------------------------ helpers


def content_hash(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:16]


def ts(value):
    """RFC3339 seconds -> naive UTC datetime (DuckDB TIMESTAMP)."""
    if value is None:
        return None
    return datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(
        timezone.utc
    ).replace(tzinfo=None)


def config_suffix(config: str) -> str:
    """'{}' -> '', '{"bits":12}' -> '@bits=12' -- the legend form of a config."""
    parsed = json.loads(config)
    if not parsed:
        return ""
    return "@" + ",".join(f"{k}={parsed[k]}" for k in sorted(parsed))


def system_key(candidate, version, config, strategy, scanner) -> str:
    return "|".join([candidate, version, config, strategy, scanner or NONE])


def system_label(candidate, strategy, scanner, config) -> str:
    label = f"{candidate}/{strategy}"
    if scanner:
        label += f"+{scanner}"
    return label + config_suffix(config)


def needle_text(needles) -> tuple[str, bool]:
    """Needles as plain text. Non-UTF-8 bytes render \\xNN and set the flag."""
    parts, binary = [], False
    for needle in needles:
        if isinstance(needle, str):
            parts.append(needle)
            continue
        raw = base64.b64decode(needle["b64"])
        try:
            parts.append(raw.decode("utf-8"))
        except UnicodeDecodeError:
            binary = True
            parts.append("".join(chr(b) if 32 <= b < 127 else f"\\x{b:02x}" for b in raw))
    return " | ".join(parts), binary


def resolve(recorded: str, fallback: Path) -> Path:
    """Prefer the path the manifest recorded; fall back to the repo layout."""
    path = Path(recorded)
    if path.exists():
        return path
    return fallback


def phase(prefilter, name):
    """Nanoseconds of one self-timed phase, or None. The JSONL also carries an
    `origin` label per phase; who held the clock follows from the column and
    the strategy, so it is not stored."""
    value = (prefilter or {}).get(name)
    return None if value is None else value["ns"]


# --------------------------------------------------------------- dimensions


def upsert(con, table, key_column, key, columns: dict):
    """Insert the row if its natural key is new, then return its surrogate id."""
    existing = con.execute(
        f"SELECT {table}_id FROM {table} WHERE {key_column} = ?", [key]
    ).fetchone()
    if existing:
        return existing[0]
    names = ", ".join([key_column, *columns])
    marks = ", ".join(["?"] * (1 + len(columns)))
    con.execute(
        f"INSERT INTO {table} ({names}) VALUES ({marks})", [key, *columns.values()]
    )
    return con.execute(
        f"SELECT {table}_id FROM {table} WHERE {key_column} = ?", [key]
    ).fetchone()[0]


def load_platform(con, env) -> int:
    key = "|".join(
        [env["hostname"], env["os"], env["arch"], env.get("cpu_model") or "", str(env["cores"])]
    )
    return upsert(
        con,
        "platform",
        "platform_key",
        key,
        {
            "hostname": env["hostname"],
            "os": env["os"],
            "arch": env["arch"],
            "cpu_model": env.get("cpu_model"),
            "cpu_features": env.get("cpu_features"),
            "cores": env["cores"],
        },
    )


def spec_content_key(manifest) -> str:
    """Identity of what was measured, path-insensitively.

    `spec_hash` hashes the spec file, so the same experiment launched from a
    different path hashes differently (the mac and x86 contains runs do). This
    key is what pairs one experiment across machines.
    """
    spec = manifest["spec"]
    payload = {
        "candidates": sorted(
            (c["name"], sorted(c["configs"])) for c in spec["candidates"]
        ),
        "scanners": sorted(s["name"] for s in spec["scanners"]),
        "strategies": sorted(spec["strategies"]),
        "measure": spec["measure"],
        "datasets": sorted(d["checksum"] for d in manifest["datasets"]),
        "suites": sorted(s["id"] for s in manifest["suites"]),
    }
    return content_hash(json.dumps(payload, sort_keys=True).encode())


def load_dataset(con, entry) -> int:
    """One dataset. Length stats come from datasets/<id>/manifest.json when it
    exists -- the artifacts are gitignored, so a fresh checkout has none."""
    raw_bytes = entry["payload_bytes"] + 8 * (entry["num_rows"] + 1)
    stats = {"min_len": None, "max_len": None, "mean_len": None}
    manifest_path = resolve(entry["path"], ROOT / "datasets" / entry["id"]) / "manifest.json"
    if manifest_path.exists():
        local = json.loads(manifest_path.read_text())
        if local["checksum"] == entry["checksum"]:
            stats = {k: local[k] for k in stats}
    return upsert(
        con,
        "dataset",
        "dataset_key",
        entry["checksum"],
        {
            "name": entry["id"],
            "checksum": entry["checksum"],
            "num_rows": entry["num_rows"],
            "payload_bytes": entry["payload_bytes"],
            "payload_mb": entry["payload_bytes"] / 1e6,
            "raw_bytes": raw_bytes,
            **stats,
        },
    )


def load_queries(con, suite_entry, dataset_id) -> int:
    """Blessed queries of one suite. Returns the number newly inserted."""
    suite = suite_entry["id"]
    path = resolve(suite_entry["path"], ROOT / "suites" / suite) / "queries.jsonl"
    rows = []
    for line in path.read_text().splitlines():
        record = json.loads(line)
        derived = record["derived"]
        text, binary = needle_text(record["needles"])
        rows.append(
            (
                f"{suite}|{record['id']}",
                suite,
                record["id"],
                dataset_id,
                record["op"],
                text,
                binary,
                derived["needle_len_total"],
                len(record["needles"]),
                derived["selectivity"],
                derived["match_count"],
            )
        )
    con.execute("CREATE OR REPLACE TEMP TABLE _q AS SELECT * FROM query LIMIT 0")
    con.executemany(
        """INSERT INTO _q (query_key, suite, query_id, dataset_id, op, needle,
                           needle_is_binary, needle_len, num_needles,
                           selectivity, match_count)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
        rows,
    )
    inserted = con.execute(
        """INSERT INTO query (query_key, suite, query_id, dataset_id, op, needle,
                              needle_is_binary, needle_len, num_needles,
                              selectivity, match_count)
           SELECT _q.query_key, suite, query_id, dataset_id, op, needle,
                  needle_is_binary, needle_len, num_needles,
                  selectivity, match_count
           FROM _q
           WHERE NOT EXISTS (SELECT 1 FROM query q WHERE q.query_key = _q.query_key)
           RETURNING 1"""
    ).fetchall()
    return len(inserted)


def load_systems(con, cells) -> dict:
    """Insert unseen systems; return {cell key -> system_id}."""
    rows = []
    for candidate, version, config, strategy, scanner in sorted(cells):
        rows.append(
            (
                system_key(candidate, version, config, strategy, scanner),
                system_label(candidate, strategy, scanner, config),
                candidate,
                version,
                config,
                strategy,
                scanner,
            )
        )
    con.execute("CREATE OR REPLACE TEMP TABLE _s AS SELECT * FROM system LIMIT 0")
    con.executemany(
        """INSERT INTO _s (system_key, label, candidate,
                           candidate_version, config, strategy, scanner)
           VALUES (?, ?, ?, ?, ?, ?, ?)""",
        rows,
    )
    con.execute(
        """INSERT INTO system (system_key, label, candidate,
                               candidate_version, config, strategy, scanner)
           SELECT _s.system_key, label, candidate,
                  candidate_version, config, strategy, scanner
           FROM _s
           WHERE NOT EXISTS (SELECT 1 FROM system s WHERE s.system_key = _s.system_key)"""
    )
    return dict(
        con.execute("SELECT system_key, system_id FROM system").fetchall()
    )


# --------------------------------------------------------------------- runs


def load_run(con, run_dir: Path) -> str:
    manifest_path = run_dir / "manifest.json"
    results_path = run_dir / "results.jsonl"
    if not manifest_path.exists():
        raise SystemExit(f"{run_dir}: no manifest.json (unfinished run?)")
    manifest = json.loads(manifest_path.read_text())
    results_bytes = results_path.read_bytes()
    results_hash = content_hash(results_bytes)

    try:
        rel = run_dir.resolve().relative_to(ROOT / "results")
    except ValueError:
        raise SystemExit(
            f"{run_dir}: outside {ROOT / 'results'} — the DB indexes that tree only"
        )
    rel = Path("results") / rel
    run_key = f"{rel}@{manifest['started_at']}"
    existing = con.execute(
        "SELECT run_id, results_hash FROM run WHERE run_key = ?", [run_key]
    ).fetchone()
    if existing and existing[1] == results_hash:
        return f"{rel}: already loaded ({results_hash})"

    platform_id = load_platform(con, manifest["env"])
    measure = manifest["spec"]["measure"]
    fields = {
        "run_dir": str(rel),
        "run_name": rel.name,
        "run_group": str(rel.parent.relative_to("results")) if rel.parent != Path("results") else "",
        "platform_id": platform_id,
        "started_at": ts(manifest["started_at"]),
        "finished_at": ts(manifest["finished_at"]),
        "duration_s": (ts(manifest["finished_at"]) - ts(manifest["started_at"])).total_seconds(),
        "git_commit": manifest["git_commit"],
        "git_dirty": manifest["git_dirty"],
        "rustc_version": manifest["env"]["rustc_version"],
        "harness_version": manifest["env"]["harness_version"],
        "cpu_governor": manifest["env"]["cpu_governor"],
        "pinned_core": manifest["pinned_core"],
        "pinning_effective": manifest["pinning_effective"],
        "warmup": measure["warmup"],
        "min_iters": measure["min_iters"],
        "min_millis": measure["min_millis"],
        "spec_hash": manifest["spec_hash"],
        "spec_content_key": spec_content_key(manifest),
        "results_hash": results_hash,
        "ingested_at": datetime.now(timezone.utc).replace(tzinfo=None),
    }
    if existing:
        # A rerun overwrote results.jsonl in place: replace this run's facts
        # rather than reporting a no-op on stale data.
        run_id = existing[0]
        con.execute("DELETE FROM result WHERE run_id = ?", [run_id])
        assignments = ", ".join(f"{k} = ?" for k in fields)
        con.execute(
            f"UPDATE run SET {assignments} WHERE run_id = ?", [*fields.values(), run_id]
        )
        note = "replaced"
    else:
        run_id = upsert(con, "run", "run_key", run_key, fields)
        note = "loaded"

    datasets = {d["checksum"]: load_dataset(con, d) for d in manifest["datasets"]}
    by_name = {d["id"]: d["checksum"] for d in manifest["datasets"]}

    # A result row names its dataset but not its suite, so the suite comes from
    # the manifest binding. More than one suite per dataset would make the
    # rows unattributable -- refuse rather than guess.
    suite_of = {}
    for entry in manifest["suites"]:
        checksum = by_name[entry["dataset"]]
        if checksum in suite_of:
            raise SystemExit(
                f"{rel}: dataset {entry['dataset']} is bound to two suites "
                f"({suite_of[checksum][0]}, {entry['id']}) -- result rows cannot be attributed"
            )
        suite_of[checksum] = (entry["id"], entry)
    new_queries = sum(
        load_queries(con, entry, datasets[checksum])
        for checksum, (_, entry) in suite_of.items()
    )
    queries = dict(con.execute("SELECT query_key, query_pk FROM query").fetchall())

    builds, cells, measurements = {}, set(), []
    for line in results_bytes.decode().splitlines():
        row = json.loads(line)
        if row["kind"] == "build":
            builds[
                (
                    row["candidate"],
                    row["candidate_version"],
                    row["config_hash"],
                    row["dataset_checksum"],
                    row["chunk_rows"],
                )
            ] = row
        elif row["kind"] == "query":
            measurements.append(row)
            cells.add(
                (
                    row["candidate"],
                    row["candidate_version"],
                    row["config"],
                    row["strategy"],
                    row.get("scanner"),
                )
            )
        else:
            # build_failed / module_unavailable: no measurement to record, but
            # never a silent absence (DESIGN.md §9).
            print(f"  note: {row['kind']} row: {json.dumps(row)}", file=sys.stderr)
    systems = load_systems(con, cells)

    rows = []
    for row in measurements:
        build_key = (
            row["candidate"],
            row["candidate_version"],
            row["config_hash"],
            row["dataset_checksum"],
            row["chunk_rows"],
        )
        build = builds.get(build_key)
        if build is None:
            raise SystemExit(f"{rel}: measured cell with no build row: {build_key}")
        latency = row.get("latency") or {}
        gate = row.get("gate") or {}
        prefilter = row.get("prefilter") or {}
        setup_ns = phase(prefilter, "setup_ns")
        decode_ns = phase(prefilter, "decode_ns")
        scan_ns = phase(prefilter, "scan_ns")
        dataset_id = datasets[row["dataset_checksum"]]
        suite = suite_of[row["dataset_checksum"]][0]
        footprint = build["footprint_total_bytes"]
        rows.append(
            (
                run_id,
                systems[
                    system_key(
                        row["candidate"],
                        row["candidate_version"],
                        row["config"],
                        row["strategy"],
                        row.get("scanner"),
                    )
                ],
                dataset_id,
                queries[f"{suite}|{row['query_id']}"],
                row["chunk_rows"],
                row["status"],
                bool(
                    row["status"] == "ok"
                    and gate.get("hash_ok")
                    and gate.get("expected_count") == gate.get("actual_count")
                ),
                row.get("error"),
                latency.get("median_ns"),
                latency.get("min_ns"),
                latency.get("p25_ns"),
                latency.get("p75_ns"),
                latency.get("p99_ns"),
                latency.get("max_ns"),
                latency.get("mean_ns"),
                latency.get("stddev_ns"),
                latency.get("samples"),
                row.get("gbps_raw"),
                # ns_per_value / ns_per_domain_value are in the JSONL too, but
                # the view derives them from total_ns rather than storing a
                # second copy of the same fact.
                row.get("eval_domain"),
                row.get("eval_domain_matches"),
                footprint,
                build["raw_bytes"],
                build["raw_bytes"] / footprint,
                build["build_ns"],
                json.dumps(build["footprint_components"]),
                prefilter.get("prefilter_candidates"),
                prefilter.get("prune_rate"),
                prefilter.get("false_positive_rate"),
                prefilter.get("verify_ns_per_survivor"),
                setup_ns,
                decode_ns,
                scan_ns,
            )
        )
    # Placeholders are generated from the column list: hand-counting them is
    # how a column added here silently shifts every value one to the left.
    columns = """run_id, system_id, dataset_id, query_pk, chunk_rows,
                 status, gate_ok, error,
                 total_ns, min_ns, p25_ns, p75_ns, p99_ns, max_ns, mean_ns,
                 stddev_ns, samples, gbps,
                 eval_domain, eval_domain_matches,
                 compressed_bytes, raw_bytes, compression_ratio, build_ns,
                 footprint_components,
                 prefilter_candidates, prune_rate, false_positive_rate,
                 verify_ns_per_survivor,
                 setup_ns, decode_ns, scan_ns"""
    marks = ", ".join(["?"] * len(columns.split(",")))
    if rows and len(rows[0]) != len(columns.split(",")):
        raise SystemExit(
            f"result row has {len(rows[0])} values for "
            f"{len(columns.split(','))} columns"
        )
    con.executemany(f"INSERT INTO result ({columns}) VALUES ({marks})", rows)

    # A rerun into the same directory is a new run row, so re-point is_latest.
    con.execute(
        """UPDATE run SET is_latest =
               (started_at = (SELECT max(started_at) FROM run older
                              WHERE older.run_dir = run.run_dir))
           WHERE run_dir = ?""",
        [str(rel)],
    )

    stored = con.execute(
        "SELECT count(*) FROM result WHERE run_id = ?", [run_id]
    ).fetchone()[0]
    if stored != len(measurements):
        raise SystemExit(
            f"{rel}: stored {stored} rows for {len(measurements)} measurements"
        )
    return (
        f"{rel}: {note} {stored} measurements, {len(builds)} builds, "
        f"{len(cells)} systems, {new_queries} new queries"
    )


def main(argv):
    if not argv:
        raise SystemExit(__doc__)
    if argv == ["--all"]:
        dirs = sorted(p.parent for p in (ROOT / "results").glob("**/manifest.json"))
    else:
        dirs = [Path(a).resolve() for a in argv]
    con = duckdb.connect(DB_PATH)
    con.execute(SCHEMA_PATH.read_text())
    for run_dir in dirs:
        print(load_run(con, run_dir))
    con.close()


if __name__ == "__main__":
    main(sys.argv[1:])
