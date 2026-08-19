#!/usr/bin/env python3
"""Summarize an onpair-spiral-prefilter run, or compare two.

  summarize_prefilter_run.py results/<run>                 # one-run summary
  summarize_prefilter_run.py results/<before> results/<after>   # A/B compare

Per strategy: geomean + total of per-query median_ns over the gated `query`
rows, split into selectivity bands (the prefilter's behavior differs sharply
by how many rows survive). The compare mode joins on (strategy, query_id) and
reports the ratio of medians.
"""
import json
import math
import sys
from collections import defaultdict
from pathlib import Path


def load(run_dir):
    rows = {}
    path = Path(run_dir) / "results.jsonl"
    if not path.exists():
        # fall back to partials from an interrupted run
        parts = sorted((Path(run_dir) / "partials").glob("*.jsonl"))
        lines = [l for p in parts for l in p.read_text().splitlines()]
    else:
        lines = path.read_text().splitlines()
    for line in lines:
        r = json.loads(line)
        if r.get("kind") != "query" or r.get("status") != "ok":
            continue
        rows[(r["strategy"], r["query_id"])] = r
    return rows


def band(r):
    s = r["derived"]["selectivity"]
    if s == 0:
        return "sel=0"
    if s < 1e-4:
        return "sel<1e-4"
    if s < 1e-2:
        return "sel<1e-2"
    return "sel>=1e-2"


BANDS = ["sel=0", "sel<1e-4", "sel<1e-2", "sel>=1e-2", "ALL"]


def geomean(xs):
    return math.exp(sum(math.log(x) for x in xs) / len(xs)) if xs else float("nan")


def summarize(rows):
    per = defaultdict(lambda: defaultdict(list))
    for (strat, _), r in rows.items():
        per[strat][band(r)].append(r["latency"]["median_ns"])
        per[strat]["ALL"].append(r["latency"]["median_ns"])
    return per


def fmt_ns(ns):
    return f"{ns / 1e6:9.3f}ms" if ns >= 1e6 else f"{ns / 1e3:9.1f}us"


if len(sys.argv) == 2:
    per = summarize(load(sys.argv[1]))
    for strat in sorted(per):
        print(f"\n{strat}")
        for b in BANDS:
            xs = per[strat][b]
            if xs:
                print(f"  {b:9} n={len(xs):3}  geomean={fmt_ns(geomean(xs))}  "
                      f"total={fmt_ns(sum(xs))}")
else:
    before, after = load(sys.argv[1]), load(sys.argv[2])
    keys = sorted(set(before) & set(after))
    missing = len(set(before) ^ set(after))
    if missing:
        print(f"warning: {missing} cells present in only one run", file=sys.stderr)
    per = defaultdict(lambda: defaultdict(list))
    for k in keys:
        b, a = before[k], after[k]
        r = a["latency"]["median_ns"] / max(b["latency"]["median_ns"], 1)
        per[k[0]][band(b)].append((r, b, a))
        per[k[0]]["ALL"].append((r, b, a))
    for strat in sorted(per):
        print(f"\n{strat}  (after/before median ratio; <1 is faster)")
        for bnd in BANDS:
            xs = per[strat][bnd]
            if not xs:
                continue
            ratios = [x[0] for x in xs]
            tb = sum(x[1]["latency"]["median_ns"] for x in xs)
            ta = sum(x[2]["latency"]["median_ns"] for x in xs)
            print(f"  {bnd:9} n={len(xs):3}  geomean-ratio={geomean(ratios):6.3f}  "
                  f"total {fmt_ns(tb)} -> {fmt_ns(ta)}  ({ta / tb:5.3f}x)")
        worst = sorted(per[strat]["ALL"], key=lambda x: -x[0])[:3]
        best = sorted(per[strat]["ALL"], key=lambda x: x[0])[:3]
        for tag, lst in (("slowest-3", worst), ("fastest-3", best)):
            print(f"    {tag}: " + "; ".join(
                f"{x[1]['query_id'].split('.', 1)[1]} {x[0]:.2f}x" for x in lst))
