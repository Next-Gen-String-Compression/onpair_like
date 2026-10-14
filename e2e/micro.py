#!/usr/bin/env python3
"""Runs the micro benchmark's needles end to end: one
`SELECT count(*) FROM <column> WHERE data LIKE '%<needle>%'` per needle of a
micro spec's suites, on the four systems of e2e/run.py, best of `runs`.

    e2e/micro.py [--spec fast] [--runs 5] [--threads 1] [--name NAME]

Reads the columns e2e/data.sh writes into $E2E_DATA (micro.duckdb and
micro.{0861,onpair}.vortex) and the suites paper/specs/micro/<spec>.toml
names. A needle is left out when it is not valid UTF-8, which a VARCHAR
literal cannot hold, or holds `%`, `_` or a backslash: vortex-duckdb turns
contains(x, n) into LIKE '%n%' without escaping n. Every count is checked
against the suite's truth. One shell per system and column; each needle's
first run is untimed and gives the count, each of the `runs` after it writes
its profile. Writes results/e2e/<name>/:

    needles.csv    system, dataset, query, needle_len, selectivity, matches,
                   run, time_s, latency_s, scan_s, payload_bytes
    manifest.json  pins, binary versions, settings, skipped needles, mismatches
    profiles/<system>/<dataset>/<query>_<run>.json

time_s is the shell's wall time, latency_s the profile's, scan_s the summed
time of its TABLE_SCAN operators.
"""

import argparse
import base64
import csv
import datetime
import json
import os
import platform
import re
import subprocess
import tomllib
from pathlib import Path

from run import BIN, DATA, E2E, ROOT, SEGMENT_CACHE_MB, SYSTEMS, parse

SPECS = ROOT / "paper" / "specs" / "micro"


def table(dataset):
    return dataset.removesuffix("-1mi").replace("-", "_")


def needles(spec):
    """{dataset: [query]} and the skipped (query id, reason)."""
    s = tomllib.loads((SPECS / f"{spec}.toml").read_text())
    out, skipped = {}, []
    for suite in s["suites"]:
        path = (SPECS / suite["path"]).resolve()
        dataset = json.loads((path / "suite.json").read_text())["dataset"]["id"]
        for line in (path / "queries.jsonl").read_text().splitlines():
            q = json.loads(line)
            n = q["needles"][0]
            raw = base64.b64decode(n["b64"]) if isinstance(n, dict) else n.encode()
            try:
                text = raw.decode()
            except UnicodeDecodeError:
                skipped.append((q["id"], "not utf-8"))
                continue
            if re.search(r"[%_\\]", text):
                skipped.append((q["id"], "% _ or \\"))
                continue
            out.setdefault(dataset, []).append({**q, "needle": text})
    return out, skipped


def run_shell(system, dataset, queries, runs, threads, out):
    binary, settings, tag = SYSTEMS[system]
    t = table(dataset)
    source = f"read_vortex('{DATA}/micro.{tag}.vortex/{t}.vortex')" if tag else t
    profile = out / "profiles" / system / dataset
    profile.mkdir(parents=True, exist_ok=True)
    lines = [f"SET threads={threads};", "PRAGMA disable_progress_bar;", *settings, ".mode csv", ".headers off"]
    for i, q in enumerate(queries):
        literal = q["needle"].replace("'", "''")
        sql = f"SELECT {i}, count(*) FROM {source} WHERE data LIKE '%{literal}%';"
        lines += [".output", sql, ".output /dev/null", "PRAGMA enable_profiling='json';"]
        for r in range(runs):
            lines += [f"PRAGMA profiling_output='{profile}/{q['id'].split('.')[-1]}_{r}.json';",
                      ".timer on", sql, ".timer off"]
        lines += ["PRAGMA disable_profiling;"]
    args = [str(BIN / binary)] + ([] if tag else ["-readonly", str(DATA / "micro.duckdb")])
    env = {**os.environ, "VORTEX_SEGMENT_CACHE_MB": str(SEGMENT_CACHE_MB)} if tag else None
    proc = subprocess.run(args, input="\n".join(lines) + "\n", capture_output=True, text=True, env=env)
    (out / "logs").mkdir(exist_ok=True)
    (out / "logs" / f"{system}-{dataset}.err").write_text(proc.stderr)
    times = [float(x) for x in re.findall(r"Run Time \(s\): real ([0-9.]+)", proc.stdout)]
    counts = dict(re.findall(r"^(\d+),(\d+)$", proc.stdout, re.M))
    if len(times) != len(queries) * runs or len(counts) != len(queries):
        raise SystemExit(f"{system}/{dataset}: {len(times)} timed runs and {len(counts)} counts for "
                         f"{len(queries)} needles; see logs/{system}-{dataset}.err")
    return [(int(counts[str(i)]), times[i * runs:(i + 1) * runs]) for i in range(len(queries))]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--spec", default="fast")
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--threads", type=int, default=1)
    arch = "neon" if platform.machine() == "arm64" else platform.machine()
    ap.add_argument("--name")
    args = ap.parse_args()
    name = args.name or f"e2e-micro-{args.spec}-{arch}-{datetime.date.today()}"
    out = ROOT / "results" / "e2e" / name
    out.mkdir(parents=True, exist_ok=True)

    suites, skipped = needles(args.spec)
    payload = {d: json.loads((ROOT / "datasets" / d / "manifest.json").read_text())["payload_bytes"] for d in suites}
    rows, mismatches = [], []
    for dataset, queries in suites.items():
        for system in SYSTEMS:
            start = datetime.datetime.now()
            for q, (count, times) in zip(queries, run_shell(system, dataset, queries, args.runs, args.threads, out)):
                qid = q["id"].split(".")[-1]
                if count != q["truth"]["count"]:
                    mismatches.append(f"{system}/{dataset}/{qid}: {count}, truth {q['truth']['count']}")
                for r, t in enumerate(times):
                    latency, scans = parse(out / "profiles" / system / dataset / f"{qid}_{r}.json")
                    rows.append([system, dataset, qid, len(q["needle"].encode()), q["derived"]["selectivity"],
                                 count, r, t, latency, sum(s[2] for s in scans), payload[dataset]])
            print(f"{dataset:22} {system:16} {len(queries):4} needles {(datetime.datetime.now() - start).seconds:5} s",
                  flush=True)

    with open(out / "needles.csv", "w", newline="") as f:
        csv.writer(f).writerows([["system", "dataset", "query", "needle_len", "selectivity", "matches", "run",
                                  "time_s", "latency_s", "scan_s", "payload_bytes"], *rows])
    pins = dict(re.findall(r"^(\w+)=(\S+)", (ROOT / "pins.env").read_text(), re.M))
    versions = {b: subprocess.run([str(BIN / b), "-noheader", "-list", "-c", "SELECT version()"],
                                  capture_output=True, text=True).stdout.strip()
                for b in sorted({s[0] for s in SYSTEMS.values()})}
    manifest = {"name": name, "date": str(datetime.date.today()), "machine": platform.machine(),
                "cpu": subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True,
                                      text=True).stdout.strip(),
                "spec": args.spec, "threads": args.threads, "runs": args.runs, "pins": pins, "versions": versions,
                "data_dir": str(DATA), "settings": {s: v[1] for s, v in SYSTEMS.items()},
                "segment_cache_mb": SEGMENT_CACHE_MB,
                "needles": {d: len(q) for d, q in suites.items()},
                "skipped": [f"{q}: {why}" for q, why in skipped], "mismatches": mismatches}
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"wrote {out}: {sum(len(q) for q in suites.values())} needles, {len(skipped)} skipped, "
          f"{len(mismatches)} counts differing from the truth")
    for m in mismatches:
        print(f"  differs  {m}")


if __name__ == "__main__":
    main()
