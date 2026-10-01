#!/usr/bin/env python3
"""Turn like_predicates.csv (LIKE predicates mined from GitHub repos running
SQLStorm-style queries) into curated benchmark suites.

Only predicates whose target column exists as an ingested dataset become
queries; everything else is dropped and accounted for. Duplicated predicates
(the same needle used by many repos/queries) are deduplicated into one query
carrying `use_count`, so results can be frequency-weighted afterwards.

Mapping to the contract (contract/SEMANTICS.md — byte-exact, case-sensitive):
  contains/prefix/suffix        -> the op, needle as written
  like/ilike '%a%'/'a%'/'%a'    -> contains / prefix / suffix
  like/ilike '%a%b%'            -> multi_contains
  ilike                         -> executed case-sensitively,
                                   meta.search_types records the fold
  NOT LIKE                      -> normalized positive, meta.negated_uses
  '%' match-all, '_' wildcards, anchored infix ('Brand%G'), bare equality
                                -> dropped (no contract op)

Usage (from the repo root, after `datasets/prepare.py --dataset imdb-keyword
--dataset imdb-akaname` and the default tpch-pname-sf10):
  python3 datasets/SQLStorm/make_like_suites.py
  target/release/bench bless --suite suites/imdb-keyword-sqlstorm \
      --dataset datasets/imdb-keyword       # …and the other suites
"""
from __future__ import annotations

import argparse
import csv
import json
import re
from collections import Counter
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent

# (table, column) -> dataset id; the only columns with an ingested corpus.
COLUMN_TO_DATASET = {
    ("keyword", "keyword"): "imdb-keyword",
    ("aka_name", "name"): "imdb-akaname",
    ("part", "p_name"): "tpch-pname-sf10",
}

OP_ORDER = {"prefix": 0, "suffix": 1, "contains": 2, "multi_contains": 3}


def classify(search_type: str, needle: str) -> tuple[str, list[str]] | str:
    """Return (op, needles) or a drop-reason string."""
    if search_type in ("contains", "prefix", "suffix") and not re.search(r"[%_]", needle):
        return search_type, [needle]
    # Everything else (like/ilike, or a wildcard that leaked into another
    # search_type) is parsed as a LIKE pattern.
    if "_" in needle:
        return "underscore-wildcard"
    segments = [s for s in needle.split("%") if s]
    if not segments:
        return "match-all"
    leading, trailing = needle.startswith("%"), needle.endswith("%")
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
    # Case is preserved: matching is case-sensitive, so Action != action.
    s = re.sub(r"[^A-Za-z0-9]+", "-", "-".join(needles)).strip("-")
    return s or "needle"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--csv", type=Path, default=REPO_ROOT / "like_predicates.csv")
    ap.add_argument("--out", type=Path, default=REPO_ROOT / "suites")
    args = ap.parse_args()

    rows = list(csv.DictReader(open(args.csv, encoding="utf-8")))
    dropped: Counter[str] = Counter()
    # (dataset, op, needles) -> merged provenance
    merged: dict[tuple[str, str, tuple[str, ...]], dict] = {}

    for r in rows:
        ds = COLUMN_TO_DATASET.get((r["table_name"], r["column_name"]))
        if ds is None:
            dropped["no-corpus-column"] += 1
            continue
        got = classify(r["search_type"], r["needle"])
        if isinstance(got, str):
            dropped[got] += 1
            continue
        op, needles = got
        m = merged.setdefault(
            (ds, op, tuple(needles)),
            {"use_count": 0, "negated_uses": 0, "search_types": set(),
             "patterns": set(), "repos": set()},
        )
        m["use_count"] += 1
        m["negated_uses"] += r["negated"] == "True"
        m["search_types"].add(r["search_type"])
        m["patterns"].add(r["needle"])
        m["repos"].add(r["repo_url"])

    per_dataset: dict[str, list] = {}
    for (ds, op, needles), m in merged.items():
        per_dataset.setdefault(ds, []).append((op, needles, m))

    for ds, entries in sorted(per_dataset.items()):
        suite_id = f"{ds}-sqlstorm"
        out_dir = args.out / suite_id
        out_dir.mkdir(parents=True, exist_ok=True)
        entries.sort(key=lambda e: (OP_ORDER[e[0]], e[1]))
        seen_ids: set[str] = set()
        lines = []
        for op, needles, m in entries:
            qid = f"sqlstorm.{op}.{slug(list(needles))}"
            while qid in seen_ids:
                qid += "-x"
            seen_ids.add(qid)
            lines.append(json.dumps({
                "id": qid,
                "op": op,
                "needles": list(needles),
                "meta": {
                    "use_count": m["use_count"],
                    "negated_uses": m["negated_uses"],
                    "search_types": sorted(m["search_types"]),
                    "source_patterns": sorted(m["patterns"]),
                    "source_repos": len(m["repos"]),
                },
            }, ensure_ascii=False))
        (out_dir / "queries.jsonl").write_text("\n".join(lines) + "\n")
        manifest = {
            "format_version": 1,
            "id": suite_id,
            "description": (
                f"Curated from like_predicates.csv (GitHub-crawled SQLStorm-style "
                f"LIKE predicates) against the {ds} column. Deduplicated; "
                f"meta.use_count preserves crawl frequency; ilike folded to "
                f"case-sensitive per the contract."
            ),
            "dataset": {"id": ds},
            "provenance": {
                "curated_by": "datasets/SQLStorm/make_like_suites.py",
                "source_csv": str(args.csv.name),
            },
        }
        (out_dir / "suite.json").write_text(json.dumps(manifest, indent=2) + "\n")
        print(f"{suite_id}: {len(lines)} queries "
              f"({sum(m['use_count'] for _, _, m in entries)} predicate uses)")

    print(f"\n{len(rows)} csv rows -> {sum(len(e) for e in per_dataset.values())} "
          f"distinct queries; dropped: {dict(dropped)}")


if __name__ == "__main__":
    main()
