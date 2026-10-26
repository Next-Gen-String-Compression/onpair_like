#!/usr/bin/env python3
"""Runs every JOB query, and with `--workloads clickbench,job` every ClickBench
query too, on the six systems of the end-to-end benchmark, best of `runs`, and
keeps the plan of the fastest run. The paper's end-to-end figure is JOB's;
ClickBench needs the full table, e2e/data.sh with E2E_CLICKBENCH=1.

    duckdb            the fork, FSST vectors on, compressed contains off
    duckdb-contains   the fork, FSST vectors on, compressed contains on
    duckdb-eq, duckdb-contains-eq   the same two with compressed equality on
    vortex            duckdb-vortex with vortex-duckdb 0.86.1
    vortex-onpair     duckdb-vortex with the OnPair revision

    e2e/run.py [--workloads job|clickbench,job] [--runs 5] [--threads 4] [--name NAME]

Reads the binaries from e2e/build.sh and the files from e2e/data.sh
($E2E_DATA, default e2e/data). One shell per system and workload, so the
buffers are warm after each query's first run (for Vortex, a segment cache
as large as DuckDB's default memory limit), which is not timed and whose
result is kept for the cross-system check. Each of the `runs` after it is
timed by the shell and writes its profile, the EXPLAIN ANALYZE plan as JSON.
Writes results/e2e/<name>/:

    queries.csv    system, workload, query, run, time_s, latency_s, scan_s
    scans.csv      system, workload, query, run, scan, table, filters, seconds
    manifest.json  pins, binary versions, data files, settings, mismatches
    profiles/<system>/<workload>/<query>_<run>.json   every run's plan
    plans/<system>/<workload>/<query>.json            the fastest run's plan
    results/, logs/

time_s is the shell's wall time of the run, in whole milliseconds, latency_s
the latency its profile reports, and scan_s the summed time of its TABLE_SCAN
operators, CPU time over all threads. The fastest run is the one with the
lowest latency_s. ClickBench q00 and q06 are answered from metadata and write
no profile, so they have time_s alone and no plan.
"""

import argparse
import csv
import datetime
import json
import os
import platform
import re
import shutil
import subprocess
from pathlib import Path

E2E = Path(__file__).resolve().parent
ROOT = E2E.parent
BIN = E2E / "builds" / "bin"
QUERIES = E2E.parent / "third_party" / "duckdb" / "benchmark"
DATA = Path(os.environ.get("E2E_DATA", E2E / "data"))
# DuckDB's default memory_limit, 80% of RAM
SEGMENT_CACHE_MB = os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES") * 8 // 10 >> 20
JOB_TABLES = re.findall(r"^CREATE TABLE (\w+)", (QUERIES / "imdb_plan_cost/init/schema.sql").read_text(), re.M)

DUCKDB = ["SET enable_fsst_vectors=true;", "SET enable_fsst_eq=false;"]
SYSTEMS = {
    "duckdb": ("duckdb", DUCKDB + ["SET enable_fsst_contains=false;"], None),
    "duckdb-contains": ("duckdb", DUCKDB + ["SET enable_fsst_contains=true;"], None),
    "vortex": ("duckdb-vortex-0.86.1", [], "0861"),
    "vortex-onpair": ("duckdb-vortex-onpair", [], "onpair"),
}
# The DuckDB pair again with compressed equality on in both arms; e2e/micro.py runs SYSTEMS only.
DUCKDB_EQ = ["SET enable_fsst_vectors=true;", "SET enable_fsst_eq=true;"]
ARMS = {
    **SYSTEMS,
    "duckdb-eq": ("duckdb", DUCKDB_EQ + ["SET enable_fsst_contains=false;"], None),
    "duckdb-contains-eq": ("duckdb", DUCKDB_EQ + ["SET enable_fsst_contains=true;"], None),
}
WORKLOADS = {
    "clickbench": (sorted((QUERIES / "clickbench/queries").glob("q[0-9]*.sql")), "ch.contains.duckdb"),
    "job": (sorted((QUERIES / "imdb_plan_cost/queries").glob("*.sql")), "imdb.pf.duckdb"),
}


def views(workload, tag):
    if workload == "clickbench":
        return [f"CREATE VIEW hits AS FROM read_vortex('{DATA}/hits.{tag}.vortex');"]
    return [f"CREATE VIEW {t} AS FROM read_vortex('{DATA}/imdb.{tag}.vortex/{t}.vortex');" for t in JOB_TABLES]


def run_shell(system, workload, runs, threads, out):
    binary, settings, tag = ARMS[system]
    files, db = WORKLOADS[workload]
    lines = [f"SET threads={threads};", "PRAGMA disable_progress_bar;", *settings,
             *(views(workload, tag) if tag else []), ".mode csv"]
    for f in files:
        sql = " ".join(f.read_text().split()).rstrip(";") + ";"
        result = out / "results" / system / workload / f"{f.stem}.csv"
        profile = out / "profiles" / system / workload
        result.parent.mkdir(parents=True, exist_ok=True)
        profile.mkdir(parents=True, exist_ok=True)
        lines += [f".output {result}", sql, ".output /dev/null", "PRAGMA enable_profiling='json';"]
        for r in range(runs):
            lines += [f"PRAGMA profiling_output='{profile}/{f.stem}_{r}.json';", ".timer on", sql, ".timer off"]
        lines += ["PRAGMA disable_profiling;"]
    args = [str(BIN / binary)] + (["-readonly", str(DATA / db)] if not tag else [])
    env = {**os.environ, "VORTEX_SEGMENT_CACHE_MB": str(SEGMENT_CACHE_MB)} if tag else None
    proc = subprocess.run(args, input="\n".join(lines) + "\n", capture_output=True, text=True, env=env)
    (out / "logs").mkdir(exist_ok=True)
    (out / "logs" / f"{system}-{workload}.err").write_text(proc.stderr)
    # The timer prints to stdout even under .output, one line per timed run.
    times = [float(t) for t in re.findall(r"Run Time \(s\): real ([0-9.]+)", proc.stdout)]
    if len(times) != len(files) * runs:
        raise SystemExit(f"{system}/{workload}: {len(times)} timed runs, expected {len(files) * runs}; "
                         f"see logs/{system}-{workload}.err")
    return {f.stem: times[i * runs:(i + 1) * runs] for i, f in enumerate(files)}


def parse(path):
    """(latency, [(table, filters, seconds)]) from either profile layout: the
    fork's (`query.total_time`, `operator`, `type`/`timing`) or v1.5.5's
    (`latency`, `children`, `operator_type`/`operator_timing`)."""
    d = json.loads(path.read_text())
    latency = d["query"]["total_time"] if "operator" in d else d["latency"]
    scans = []

    def walk(n):
        if n.get("type", n.get("operator_type")) == "TABLE_SCAN":
            info = n.get("extra_info", {})
            table = info.get("Table", "").split(".")[-1] or Path(info.get("Filename(s)", "")).stem
            filters = info.get("Filters", "")
            filters = " AND ".join(filters) if isinstance(filters, list) else filters
            scans.append((table, filters, n.get("timing", n.get("operator_timing"))))
        for c in n.get("children", []):
            walk(c)

    for n in d.get("operator", d.get("children", [])):
        walk(n)
    return latency, scans


def same_result(a, b):
    return a.exists() and b.exists() and sorted(a.read_text().splitlines()) == sorted(b.read_text().splitlines())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--workloads", default="job", help="comma-separated, of " + ", ".join(WORKLOADS))
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--threads", type=int, default=4)
    arch = "neon" if platform.machine() in ("arm64", "aarch64") else platform.machine()
    ap.add_argument("--name", default=f"e2e-{arch}-{datetime.date.today()}")
    args = ap.parse_args()
    chosen = args.workloads.split(",")
    if unknown := set(chosen) - set(WORKLOADS):
        ap.error(f"unknown workload {', '.join(sorted(unknown))}; there are {', '.join(WORKLOADS)}")
    workloads = {w: v for w, v in WORKLOADS.items() if w in chosen}
    out = ROOT / "results" / "e2e" / args.name
    out.mkdir(parents=True, exist_ok=True)

    rows, scan_rows, missing = [], [], []
    for workload in workloads:
        for system in ARMS:
            start = datetime.datetime.now()
            plans = out / "plans" / system / workload
            plans.mkdir(parents=True, exist_ok=True)
            for q, times in run_shell(system, workload, args.runs, args.threads, out).items():
                profiles = out / "profiles" / system / workload
                latencies = {}
                for r, t in enumerate(times):
                    path = profiles / f"{q}_{r}.json"
                    if not path.exists():
                        rows.append([system, workload, q, r, t, "", ""])
                        continue
                    latencies[r], scans = parse(path)
                    rows.append([system, workload, q, r, t, latencies[r], sum(s[2] for s in scans)])
                    scan_rows += [[system, workload, q, r, i, *s] for i, s in enumerate(scans)]
                if latencies:
                    fastest = min(latencies, key=latencies.get)
                    shutil.copyfile(profiles / f"{q}_{fastest}.json", plans / f"{q}.json")
                else:
                    missing.append(f"{system}/{workload}/{q}")
            print(f"{workload:10} {system:16} {(datetime.datetime.now() - start).seconds:5} s", flush=True)

    with open(out / "queries.csv", "w", newline="") as f:
        csv.writer(f).writerows([["system", "workload", "query", "run", "time_s", "latency_s", "scan_s"], *rows])
    with open(out / "scans.csv", "w", newline="") as f:
        csv.writer(f).writerows([["system", "workload", "query", "run", "scan", "table", "filters", "seconds"],
                                 *scan_rows])

    # A result that differs from the DuckDB baseline's, compared as sorted lines:
    # ties under a LIMIT and float formatting differ between versions too.
    mismatches = [f"{system}/{workload}/{f.stem}"
                  for workload, (files, _) in workloads.items() for f in files for system in list(ARMS)[1:]
                  if not same_result(out / "results" / "duckdb" / workload / f"{f.stem}.csv",
                                     out / "results" / system / workload / f"{f.stem}.csv")]
    pins = dict(re.findall(r"^(\w+)=(\S+)", (ROOT / "pins.env").read_text(), re.M))
    versions = {b: subprocess.run([str(BIN / b), "-noheader", "-list", "-c", "SELECT version()"],
                                  capture_output=True, text=True).stdout.strip()
                for b in sorted({s[0] for s in ARMS.values()})}
    used = [db for _, db in workloads.values()] + [f"{w}.{tag}.vortex" for w in ("hits", "imdb")
                                                    if {"hits": "clickbench", "imdb": "job"}[w] in workloads
                                                    for _, _, tag in SYSTEMS.values() if tag]
    data = {name: sum(f.stat().st_size for f in ([p] if p.is_file() else p.iterdir()))
            for name in used if (p := DATA / name).exists()}
    manifest = {"name": args.name, "date": str(datetime.date.today()), "machine": platform.machine(),
                "cpu": subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True,
                                      text=True).stdout.strip(),
                "workloads": list(workloads),
                "threads": args.threads, "runs": args.runs, "pins": pins, "versions": versions,
                "data_dir": str(DATA), "data_bytes": data,
                "settings": {s: v[1] for s, v in ARMS.items()}, "segment_cache_mb": SEGMENT_CACHE_MB,
                "missing": missing, "mismatches": mismatches}
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"wrote {out}: {len(rows)} runs, {len(missing)} queries without a plan, "
          f"{len(mismatches)} results differing from duckdb")
    for m in missing:
        print(f"  missing  {m}")
    for m in mismatches:
        print(f"  differs  {m}")


if __name__ == "__main__":
    main()
