#!/usr/bin/env python3
"""Query-axis ranking with the per-query setup cost split out.

`headline` is what the harness clocks over the whole run() — for FSST-LIKE's
cpp/cpp-simd backends that INCLUDES a `clang++ -shared` per call, which is
~90-95% of it. `setup` is the candidate's self-reported setup_ns (ABI v3,
measured in one instrumented pass), so `match` = headline - setup is the number
that compares fairly against candidates with no per-query compile.
See DESIGN.md §17.4.

Usage: python3 scratchpad/report_setup_vs_match.py results/<run>/results.jsonl
"""
import json
import sys
from collections import Counter, defaultdict


def cell_key(r):
    cfg = r.get("config")
    cfg = "" if cfg in (None, "{}") else "/" + cfg.strip("{}").replace('"', "")
    return (r["candidate"] + cfg, r["strategy"], r.get("scanner") or "-")


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "results/msmarco-all-dictfix/results.jsonl"
    head, setup, nq = defaultdict(float), defaultdict(float), Counter()
    status = Counter()
    for line in open(path):
        r = json.loads(line)
        if r.get("kind") != "query":
            continue
        status[r["status"]] += 1
        if r["status"] != "ok":
            print(f"  !! {r['status']}: {r['candidate']}/{r['strategy']} {r['query_id']}")
            continue
        k = cell_key(r)
        head[k] += (r.get("latency") or {}).get("median_ns", 0) / 1e6
        setup[k] += ((r.get("prefilter") or {}).get("setup_ns") or {}).get("ns", 0) / 1e6
        nq[k] += 1

    queries = max(nq.values()) if nq else 0
    print(f"{path}\n{sum(status.values())} cells {dict(status)} | "
          f"{len(head)} rows x {queries} queries; suite-total ms\n")
    print(f"{'candidate':34s}{'strategy':25s}{'scan':8s}{'head':>9s}{'setup':>8s}{'match':>9s}")
    for k in sorted(head, key=lambda k: head[k]):
        h, s = head[k], setup[k]
        print(f"{k[0]:34s}{k[1]:25s}{k[2]:8s}{h:9.1f}{s:8.1f}{h - s:9.1f}")


if __name__ == "__main__":
    main()
