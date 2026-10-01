# /// script
# requires-python = ">=3.11"
# dependencies = ["duckdb>=1.1", "pandas>=2"]
# ///
"""Checks paper/inputs.toml against what results/ and paper/data hold, and
exits non-zero on any finding:

    missing   an entry that does not exist, or a run not loaded into the DB
    stale     a newer run, CSV or log of the same name with a later date
    mixed     figures drawn from different needle suites (fast, medium, full)
    labels    a micro machine whose runs lack a series plot_micro.py draws
    split     verify needles the split CSV has no row for

    uv run paper/check_inputs.py [--update]

--update rewrites every stale entry to the newest one, then checks again.
"""

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path

import duckdb
import pandas as pd

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
DB = ROOT / "results" / "bench.duckdb"
INPUTS = HERE / "inputs.toml"
DATE = r"\d{4}-\d{2}-\d{2}[a-z]?"

# The series plot_micro.SYSTEMS draws, kept here rather than imported so the
# check needs no matplotlib.
MICRO_LABELS = [
    "onpair_spiral/pf_walk@bits=12,prune=1.0",
    "onpair_spiral/pf_walk@bits=16,prune=1.0",
    "fsst_like_tum/interp",
    "fsst_prefilter/fsst-prefilter",
    "fsst/decode+memmem-hay",
    "onpair/decode+memmem-hay",
    "uncompressed/direct+memmem-hay",
]


def prefix_of(name, suffix=""):
    m = re.fullmatch(rf"(.*-|.*_){DATE}{re.escape(suffix)}", name)
    return m.group(1) if m else None


def newest(name, candidates, suffix=""):
    """The newest of `candidates` sharing `name`'s prefix, if newer than it."""
    prefix = prefix_of(name, suffix)
    if prefix is None:
        return None
    pattern = re.compile(rf"{re.escape(prefix)}{DATE}{re.escape(suffix)}")
    same = [c for c in candidates if pattern.fullmatch(c)]
    best = max(same, key=candidates.get, default=name)
    return best if best != name and candidates[best] > candidates.get(name, "") else None


def suites_of_spec(spec):
    with open(spec, "rb") as f:
        return {Path(s["path"]).name for s in tomllib.load(f)["suites"]}


def check(inputs, con):
    findings, stale = [], {}
    runs = dict(con.sql("select run_name, strftime(started_at, '%Y-%m-%d %H:%M:%S') "
                        "from run where is_latest").fetchall())
    run_dir = dict(con.sql("select run_name, run_dir from run where is_latest").fetchall())

    def run(name, what):
        if name not in runs:
            findings.append(f"missing  {what}: run {name} is not in {DB.name}")
            return False
        if (new := newest(name, runs)):
            findings.append(f"stale    {what}: {name}, newer {new}")
            stale[name] = new
        return True

    def file(rel, what, base=HERE, suffix=".csv"):
        path = base / rel
        if not path.exists():
            findings.append(f"missing  {what}: {rel}")
            return
        siblings = {p.name: p.name for p in path.parent.iterdir()}
        if (new := newest(path.name, siblings, suffix)):
            findings.append(f"stale    {what}: {rel}, newer {new}")
            stale[rel] = str(Path(rel).parent / new)

    suites = {}

    def suites_of_run(name):
        manifest = json.loads((ROOT / run_dir[name] / "manifest.json").read_text())
        return {Path(s["path"]).name for s in manifest["suites"]}

    for machine, name in inputs["micro"].items():
        if not run(name, f"micro {machine}"):
            continue
        suites[f"micro {machine}"] = suites_of_run(name)
        labels = {r[0] for r in con.sql(
            "select distinct label from v where is_latest and run_name = ?",
            params=[name]).fetchall()}
        lacking = [l for l in MICRO_LABELS if l not in labels]
        if lacking:
            findings.append(f"labels   micro {machine}: no {', '.join(lacking)}")

    if run(inputs["breakdown"]["jit"], "breakdown JIT"):
        suites["breakdown JIT"] = suites_of_run(inputs["breakdown"]["jit"])

    verify = inputs["verify"]
    if run(verify["run"], "verify"):
        suites["verify"] = suites_of_run(verify["run"])
    file(verify["split"], "verify split")
    split = HERE / verify["split"]
    if verify["run"] in runs and split.exists():
        needles = {r[0] for r in con.sql(
            "select distinct query_id from v where is_latest and run_name = ?",
            params=[verify["run"]]).fetchall()}
        covered = set(pd.read_csv(split, usecols=["query_id"])["query_id"])
        if (gap := needles - covered):
            findings.append(f"split    {verify['split']} lacks {len(gap)} of the "
                            f"{len(needles)} needles of {verify['run']}")

    for rel in inputs["kernel"]["matcher"]:
        file(rel, "matcher sweep")
    file(inputs["kernel"]["resolver"], "resolver sweep")

    log = inputs["plan"]["log"]
    file(log, "plan log", base=ROOT, suffix="")
    tag = prefix_of(Path(log).name)
    spec = HERE / "specs" / f"{tag[:-1]}.toml" if tag else None
    if spec and spec.exists():
        suites["plan"] = suites_of_spec(spec)

    # A needle suite is named <generator>-n<per cell>-<fingerprint>; the
    # per-cell count is the size (n1 fast, n10 medium, n20 full).
    sizes = {what: sorted({re.search(r"-n\d+-", s).group(0).strip("-") for s in ids})
             for what, ids in suites.items()}
    if len({tuple(v) for v in sizes.values()}) > 1:
        findings.append("mixed    needle suites: " +
                        "; ".join(f"{w} {'/'.join(v)}" for w, v in sizes.items()))
    return findings, stale


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--update", action="store_true")
    args = ap.parse_args()
    con = duckdb.connect(str(DB), read_only=True)
    with open(INPUTS, "rb") as f:
        findings, stale = check(tomllib.load(f), con)
    if args.update and stale:
        text = INPUTS.read_text()
        for old, new in stale.items():
            text = text.replace(f'"{old}"', f'"{new}"')
            print(f"updated  {old} -> {new}")
        INPUTS.write_text(text)
        with open(INPUTS, "rb") as f:
            findings, _ = check(tomllib.load(f), con)
    for line in findings:
        print(line)
    if findings:
        sys.exit(1)
    print(f"{INPUTS.relative_to(ROOT)}: every input present, newest and consistent")


if __name__ == "__main__":
    main()
