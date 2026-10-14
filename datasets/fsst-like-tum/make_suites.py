#!/usr/bin/env python3
"""Turn the FSST-LIKE-Matching (DaMoN'26) benchmark workload into suites.

patterns.json is vendored verbatim from the pinned upstream checkout
(calin2110/FSST-LIKE-Matching @ b1eb3ab — the same commit the fsst_like_tum
candidate builds, see candidates/fsst_like_tum/cpp/CMakeLists.txt). It holds
the paper's LIKE patterns for TPC-H, StackOverflow, IMDB, and PublicBI, keyed
by underscore count ("0", "1", ">=2").

What becomes a query here:
  TPCH patterns, "0" bucket only  -> a suite per column, LIKE parsed to the
                                     contract op (contains/prefix/suffix/
                                     multi_contains, byte-exact)
  "1" / ">=2" buckets             -> dropped: the contract has no `_` wildcard
  StackOverflow / IMDB / PublicBI -> dropped: no ingested corpus (their
                                     benchmark reads a private binary layout)
All drops are counted and printed.

Usage (from the repo root):
  python3 datasets/prepare.py --dataset tpch-ptype-sf10 \
      --dataset tpch-ocomment-sf10 --dataset tpch-scomment-sf10
  python3 datasets/fsst-like-tum/make_suites.py
  target/release/bench bless --suite suites/tpch-pname-sf10-fsst-like-tum \
      --dataset datasets/tpch-pname-sf10    # ...and the other three suites
"""
from __future__ import annotations

import argparse
import json
import re
from collections import Counter
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
PINS = dict(re.findall(r"^(\w+)=(\S+)", (REPO_ROOT / "pins.env").read_text(), re.M))

# (table, column) -> dataset id; all four TPC-H columns the paper queries.
COLUMN_TO_DATASET = {
    ("part", "p_name"): "tpch-pname-sf10",
    ("part", "p_type"): "tpch-ptype-sf10",
    ("orders", "o_comment"): "tpch-ocomment-sf10",
    ("supplier", "s_comment"): "tpch-scomment-sf10",
}

OP_ORDER = {"prefix": 0, "suffix": 1, "contains": 2, "multi_contains": 3}


def classify(pattern: str) -> tuple[str, list[str]] | str:
    """LIKE pattern -> (op, needles) or a drop-reason string.
    Same mapping as datasets/SQLStorm/make_like_suites.py."""
    if "_" in pattern:
        return "underscore-wildcard"
    segments = [s for s in pattern.split("%") if s]
    if not segments:
        return "match-all"
    leading, trailing = pattern.startswith("%"), pattern.endswith("%")
    if len(segments) == 1:
        if leading and trailing:
            return "contains", segments
        if trailing:
            return "prefix", segments
        if leading:
            return "suffix", segments
        return "equality"
    if leading and trailing:
        return "multi_contains", segments
    return "anchored-infix"


def slug(needles: list[str]) -> str:
    s = re.sub(r"[^A-Za-z0-9]+", "-", "-".join(needles)).strip("-")
    return s or "needle"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--patterns", type=Path,
                    default=Path(__file__).resolve().parent / "patterns.json")
    ap.add_argument("--out", type=Path, default=REPO_ROOT / "suites")
    args = ap.parse_args()

    workload = json.loads(args.patterns.read_text())
    dropped: Counter[str] = Counter()
    per_dataset: dict[str, list] = {}

    for dataset_name, buckets in workload.items():
        if dataset_name != "TPCH":
            for bucket in buckets.values():
                dropped["no-corpus-column"] += sum(
                    len(qs) for node in bucket.values()
                    for qs in ([node] if isinstance(node, list) else node.values()))
            continue
        for bucket_key, tables in buckets.items():
            for table, columns in tables.items():
                for column, queries in columns.items():
                    ds = COLUMN_TO_DATASET[(table, column)]
                    for q in queries:
                        got = classify(q["pattern"])
                        if isinstance(got, str):
                            dropped[got] += 1
                            continue
                        op, needles = got
                        per_dataset.setdefault(ds, []).append(
                            (op, needles, q["pattern"], q["query"]))

    total = 0
    for ds, entries in sorted(per_dataset.items()):
        suite_id = f"{ds}-fsst-like-tum"
        out_dir = args.out / suite_id
        out_dir.mkdir(parents=True, exist_ok=True)
        entries.sort(key=lambda e: (OP_ORDER[e[0]], e[1]))
        lines = []
        for op, needles, pattern, tpch_q in entries:
            lines.append(json.dumps({
                "id": f"fsstliketum.{op}.{slug(needles)}",
                "op": op,
                "needles": needles,
                "meta": {
                    "source_pattern": pattern,
                    "tpch_query": None if tpch_q is None else f"Q{tpch_q}",
                },
            }, ensure_ascii=False))
        (out_dir / "queries.jsonl").write_text("\n".join(lines) + "\n")
        manifest = {
            "format_version": 1,
            "id": suite_id,
            "description": (
                f"The FSST-LIKE-Matching (DaMoN'26) benchmark's TPC-H patterns "
                f"against {ds}, from the upstream repo's benchmark/patterns.json "
                f"(pinned commit b1eb3ab). Zero-underscore bucket only — the "
                f"contract has no `_` wildcard; meta.tpch_query names the TPC-H "
                f"query a pattern comes from, null for the paper's extras."
            ),
            "dataset": {"id": ds},
            "provenance": {
                "curated_by": "datasets/fsst-like-tum/make_suites.py",
                "source_json": "datasets/fsst-like-tum/patterns.json",
                "upstream": "https://github.com/calin2110/FSST-LIKE-Matching",
                "upstream_commit": PINS["TUM_FSST_LIKE_SHA"],
            },
        }
        (out_dir / "suite.json").write_text(json.dumps(manifest, indent=2) + "\n")
        total += len(lines)
        print(f"{suite_id}: {len(lines)} queries")

    print(f"\n{total} queries emitted; dropped: {dict(dropped)}")


if __name__ == "__main__":
    main()
