#!/usr/bin/env python3
"""Contains-runtime-by-needle-length markdown table (median full-column scan,
ms), matching the teammate's manual format. Reads a bench results.jsonl.

Usage: python3 contains_by_needle_len.py results/paper/clickbench-url-1m-contains/results.jsonl
"""
import json
import sys
from collections import defaultdict
from statistics import median

LENS = [1, 2, 4, 8, 16, 32, 64]


def main():
    path = sys.argv[1]
    rows = [json.loads(l) for l in open(path) if l.strip()]
    q = [
        r
        for r in rows
        if r.get("kind") == "query"
        and r.get("op") == "contains"
        and r.get("chunk_rows") == 0
        and r.get("status") == "ok"
    ]

    by_engine_len = defaultdict(lambda: defaultdict(list))
    for r in q:
        engine = f"{r['candidate']}:{r['strategy']}"
        nlen = r.get("derived", {}).get("needle_len_total")
        if nlen not in LENS:
            continue
        by_engine_len[engine][nlen].append(r["latency"]["median_ns"] / 1e6)

    engines = sorted(by_engine_len)
    header = "| engine | " + " | ".join(str(n) for n in LENS) + " |"
    sep = "| --- | " + " | ".join("---:" for _ in LENS) + " |"
    print(header)
    print(sep)
    for e in engines:
        cells = []
        for n in LENS:
            vals = by_engine_len[e].get(n)
            cells.append(f"{median(vals):.1f}" if vals else "-")
        print(f"| {e} | " + " | ".join(cells) + " |")


if __name__ == "__main__":
    main()
