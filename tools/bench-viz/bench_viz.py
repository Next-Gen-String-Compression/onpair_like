#!/usr/bin/env python3
"""Build a self-contained interactive viewer from benchmark results.jsonl files.

The browser owns presentation and interaction; this small, dependency-free
front end only validates and normalizes the harness result rows, then embeds
them alongside the checked-in HTML/CSS/JavaScript assets.
"""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Sequence


HERE = Path(__file__).resolve().parent
DEFAULT_OUT = HERE / "out" / "index.html"


def finite_number(value: Any) -> Optional[float]:
    if isinstance(value, bool):
        return None
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    return number if math.isfinite(number) else None


def resolve_results_path(path: Path) -> Path:
    return path / "results.jsonl" if path.is_dir() else path


def source_label(path: Path) -> str:
    if path.name == "results.jsonl":
        return path.parent.name
    return path.stem


def iter_rows(path: Path) -> Iterable[Dict[str, Any]]:
    with path.open(encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, 1):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError as error:
                raise ValueError(f"{path}:{line_number}: invalid JSON: {error}") from error
            if not isinstance(row, dict):
                raise ValueError(f"{path}:{line_number}: expected a JSON object")
            yield row


def normalize_query(row: Dict[str, Any], source: str) -> Optional[Dict[str, Any]]:
    if row.get("kind") != "query" or row.get("status") != "ok":
        return None

    latency = row.get("latency") or {}
    derived = row.get("derived") or {}
    prefilter = row.get("prefilter") or {}
    decode = prefilter.get("decode_ns") or {}

    median_ns = finite_number(latency.get("median_ns"))
    ns_per_row = finite_number(row.get("ns_per_row"))
    gbps = finite_number(row.get("gbps_raw"))
    decode_ns = finite_number(decode.get("ns"))
    selectivity = finite_number(derived.get("selectivity"))
    needle_len = finite_number(derived.get("needle_len_total"))

    # Older suites may only carry the requested target length in meta.gen.
    if needle_len is None:
        meta = row.get("meta") or {}
        needle_len = finite_number((meta.get("gen") or {}).get("target_len"))

    decode_gbps = None
    decode_ns_per_row = None
    if decode_ns is not None and decode_ns > 0 and median_ns is not None:
        # gbps_raw = raw_payload_bytes / median_ns numerically (bytes/ns == GB/s).
        if gbps is not None:
            decode_gbps = gbps * median_ns / decode_ns
        if ns_per_row is not None and ns_per_row > 0:
            num_rows = median_ns / ns_per_row
            if num_rows > 0:
                decode_ns_per_row = decode_ns / num_rows

    return {
        "source": source,
        "candidate": str(row.get("candidate", "unknown")),
        "config": str(row.get("config", "{}")),
        "config_hash": str(row.get("config_hash", "")),
        "strategy": str(row.get("strategy", "unknown")),
        "scanner": row.get("scanner"),
        "dataset": str(row.get("dataset", "unknown")),
        "chunk_rows": int(row.get("chunk_rows", 0) or 0),
        "op": str(row.get("op", "unknown")),
        "query_id": str(row.get("query_id", "")),
        "selectivity": selectivity,
        "needle_len": needle_len,
        "gbps": gbps,
        "ns_per_row": ns_per_row,
        "latency_ns": median_ns,
        # A decode-only baseline is valid only for the harness-composed decode
        # strategy. Other strategies can self-report a decode phase, but that is
        # attribution inside the algorithm rather than full-column decompression.
        "decode_gbps": decode_gbps if row.get("strategy") == "decode" else None,
        "decode_ns_per_row": decode_ns_per_row if row.get("strategy") == "decode" else None,
    }


def load_results(paths: Sequence[Path]) -> List[Dict[str, Any]]:
    points: List[Dict[str, Any]] = []
    for requested in paths:
        path = resolve_results_path(requested)
        if not path.is_file():
            raise FileNotFoundError(f"results file not found: {path}")
        label = source_label(path)
        for row in iter_rows(path):
            point = normalize_query(row, label)
            if point is not None:
                points.append(point)
    if not points:
        joined = ", ".join(str(path) for path in paths)
        raise ValueError(f"no successful query rows found in: {joined}")
    return points


def json_for_script(value: Any) -> str:
    # Prevent a query/config string containing </script> from ending the data tag.
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False).replace("<", "\\u003c")


def build_html(points: Sequence[Dict[str, Any]], defaults: Dict[str, Any]) -> str:
    template = (HERE / "template.html").read_text(encoding="utf-8")
    css = (HERE / "app.css").read_text(encoding="utf-8")
    javascript = (HERE / "app.js").read_text(encoding="utf-8")
    replacements = {
        "__BENCH_VIZ_CSS__": css,
        "__BENCH_VIZ_DATA__": json_for_script(list(points)),
        "__BENCH_VIZ_DEFAULTS__": json_for_script(defaults),
        "__BENCH_VIZ_JS__": javascript,
    }
    for marker, value in replacements.items():
        if marker not in template:
            raise ValueError(f"template marker missing: {marker}")
        template = template.replace(marker, value)
    return template


def parse_args(argv: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build a self-contained interactive benchmark plot viewer."
    )
    parser.add_argument(
        "results",
        nargs="+",
        type=Path,
        help="results.jsonl file or a run directory containing it; repeatable",
    )
    parser.add_argument("--out", "-o", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--title", default="Benchmark Explorer 3000™")
    parser.add_argument(
        "--subtitle",
        default="Query throughput across observed selectivity",
    )
    parser.add_argument(
        "--show",
        action="append",
        default=[],
        metavar="TEXT",
        help="initially show series whose candidate/strategy label contains TEXT; repeatable",
    )
    return parser.parse_args(argv)


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = parse_args(argv)
    try:
        points = load_results(args.results)
        html = build_html(
            points,
            {
                "title": args.title,
                "subtitle": args.subtitle,
                "show": args.show,
            },
        )
    except (OSError, ValueError) as error:
        print(f"bench-viz: {error}")
        return 2

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(html, encoding="utf-8")
    print(f"wrote {args.out} ({len(points)} successful query rows)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
