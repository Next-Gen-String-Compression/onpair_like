#!/usr/bin/env python3
"""Build a self-contained interactive viewer from benchmark results.jsonl files.

The browser owns presentation and interaction; this small, dependency-free
front end only validates and normalizes the harness result rows, then embeds
them alongside the checked-in HTML/CSS/JavaScript assets.
"""

from __future__ import annotations

import argparse
import base64
import binascii
import json
import math
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Sequence

import prefilter_model


HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parents[1]
DEFAULT_OUT = HERE / "out" / "index.html"


def finite_number(value: Any) -> Optional[float]:
    if isinstance(value, bool):
        return None
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    return number if math.isfinite(number) else None


def resolve_results_paths(path: Path) -> List[Path]:
    """Every results file a command-line argument names, in a stable order.

    A file is itself; a run directory is its `results.jsonl`. Anything else is
    searched to any depth, so a tree of runs -- including the nested groups a
    campaign accumulates -- can be loaded by naming the tree once.
    """
    if not path.is_dir():
        return [path]
    direct = path / "results.jsonl"
    if direct.is_file():
        return [direct]
    return sorted(path.rglob("results.jsonl"))


def source_label(path: Path) -> str:
    if path.name == "results.jsonl":
        return path.parent.name
    return path.stem


def source_labels(paths: Sequence[Path]) -> List[str]:
    """A distinct Run name per results file, as short as remains unambiguous.

    Two campaigns can both hold a run called `needle-sweep`, and a Run selector
    listing that name twice is worse than useless. Colliding names take on one
    more parent directory each, repeatedly, until they differ.
    """
    labels = [source_label(path) for path in paths]
    depth = {index: 1 for index in range(len(paths))}
    for _ in range(8):
        collisions = {
            label for label in labels if labels.count(label) > 1
        }
        if not collisions:
            break
        for index, label in enumerate(labels):
            if label not in collisions:
                continue
            parts = paths[index].parent.parts
            depth[index] = min(depth[index] + 1, len(parts))
            labels[index] = "/".join(parts[-depth[index]:])
    return labels


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


# This tool is about substring search: LIKE '%n%' and its multi-needle forms.
# Those are the predicates a compressed-domain prefilter can serve, and the only
# ones the analysis panels have anything to say about. Anchored matches (LIKE
# 'n%', LIKE '%n') are a different question and are left out entirely rather
# than pooled in and silently diluting every summary.
SUBSTRING_OPS = frozenset({"contains", "multi_contains", "contains_any"})


def is_substring_search(row: Dict[str, Any]) -> bool:
    return row.get("op") in SUBSTRING_OPS


def normalize_query(row: Dict[str, Any], source: str) -> Optional[Dict[str, Any]]:
    if row.get("kind") != "query" or row.get("status") != "ok":
        return None

    latency = row.get("latency") or {}
    derived = row.get("derived") or {}
    prefilter = row.get("prefilter") or {}
    gate = row.get("gate") or {}
    decode = prefilter.get("decode_ns") or {}

    def phase_ns(name: str) -> Optional[float]:
        phase = prefilter.get(name) or {}
        return finite_number(phase.get("ns")) if isinstance(phase, dict) else None

    # ABI v6 static cover facts, collected outside the measurement loop.
    # Throughput on a compressed-domain prefilter is set by how the needle
    # tokenizes, not by how many rows match, so these are the axes that
    # actually explain a result set's spread.
    comparison_cost = finite_number(prefilter.get("comparison_cost"))
    covered_fraction = finite_number(prefilter.get("covered_fraction"))
    cover_points = finite_number(prefilter.get("cover_points"))
    cover_ranges = finite_number(prefilter.get("cover_ranges"))
    candidate_rows = finite_number(prefilter.get("prefilter_candidates"))
    profitable = prefilter.get("profitable_hint")
    # The counts `covered_fraction` is a ratio of, and the share of *rows* the
    # cover admits. Verification is charged per row, so the row share is what
    # sets the verify cost -- and what the library's policy thresholds.
    covered_codes = finite_number(prefilter.get("covered_codes"))
    indexed_codes = finite_number(prefilter.get("indexed_codes"))
    candidate_row_fraction = finite_number(prefilter.get("candidate_row_fraction"))

    median_ns = finite_number(latency.get("median_ns"))
    # The harness calls this `ns_per_value` -- median over the row count. The
    # older `ns_per_row` spelling is accepted so results already on disk from
    # before the rename still plot.
    ns_per_row = finite_number(row.get("ns_per_value"))
    if ns_per_row is None:
        ns_per_row = finite_number(row.get("ns_per_row"))
    gbps = finite_number(row.get("gbps_raw"))
    decode_ns = finite_number(decode.get("ns"))
    selectivity = finite_number(derived.get("selectivity"))
    needle_len = finite_number(derived.get("needle_len_total"))
    needle_lens = derived.get("needle_lens")
    if not isinstance(needle_lens, list):
        needle_lens = []
    needle_lens = [number for value in needle_lens
                   if (number := finite_number(value)) is not None]

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
        "candidate_version": str(row.get("candidate_version") or ""),
        # Filled in by apply_labels(); absent means "build the label from the
        # candidate/config/strategy fields as before".
        "display": None,
        # Filled in by apply_dataset_labels(); absent means "use the raw name".
        "dataset_display": None,
        "config": str(row.get("config", "{}")),
        "config_hash": str(row.get("config_hash", "")),
        "strategy": str(row.get("strategy", "unknown")),
        "scanner": row.get("scanner"),
        "dataset": str(row.get("dataset", "unknown")),
        "chunk_rows": int(row.get("chunk_rows", 0) or 0),
        "op": str(row.get("op", "unknown")),
        "query_id": str(row.get("query_id", "")),
        # Results deliberately identify a query by id rather than repeating its
        # bytes for every candidate. apply_query_catalog() fills this from the
        # suite's queries.jsonl when one is available.
        "needles": None,
        "query_meta": row.get("meta") if isinstance(row.get("meta"), dict) else None,
        "selectivity": selectivity,
        "needle_len": needle_len,
        "needle_lens": needle_lens,
        "match_count": finite_number(derived.get("match_count")),
        "rarest_byte_freq": finite_number(derived.get("rarest_byte_freq")),
        "comparison_cost": comparison_cost,
        "covered_fraction": covered_fraction,
        "cover_points": cover_points,
        "cover_ranges": cover_ranges,
        "candidate_rows": candidate_rows,
        "covered_codes": covered_codes,
        "indexed_codes": indexed_codes,
        "candidate_row_fraction": candidate_row_fraction,
        "profitable": profitable if isinstance(profitable, bool) else None,
        "prune_rate": finite_number(prefilter.get("prune_rate")),
        "false_positive_rate": finite_number(prefilter.get("false_positive_rate")),
        "verify_per_survivor": finite_number(prefilter.get("verify_per_survivor")),
        "prefilter_ns": phase_ns("prefilter_ns"),
        "verify_ns": phase_ns("verify_ns"),
        "scan_ns": phase_ns("scan_ns"),
        "gate_expected_count": finite_number(gate.get("expected_count")),
        "gate_actual_count": finite_number(gate.get("actual_count")),
        "gate_hash_ok": gate.get("hash_ok") if isinstance(gate.get("hash_ok"), bool) else None,
        "gbps": gbps,
        "ns_per_row": ns_per_row,
        "latency_ns": median_ns,
        "latency_min_ns": finite_number(latency.get("min_ns")),
        "latency_p25_ns": finite_number(latency.get("p25_ns")),
        "latency_p75_ns": finite_number(latency.get("p75_ns")),
        "latency_p99_ns": finite_number(latency.get("p99_ns")),
        "latency_max_ns": finite_number(latency.get("max_ns")),
        "latency_mean_ns": finite_number(latency.get("mean_ns")),
        "latency_stddev_ns": finite_number(latency.get("stddev_ns")),
        "latency_samples": finite_number(latency.get("samples")),
        # A decode-only baseline is valid only for the harness-composed decode
        # strategy. Other strategies can self-report a decode phase, but that is
        # attribution inside the algorithm rather than full-column decompression.
        "decode_gbps": decode_gbps if row.get("strategy") == "decode" else None,
        "decode_ns_per_row": decode_ns_per_row if row.get("strategy") == "decode" else None,
    }


def normalize_needle(value: Any) -> Dict[str, Any]:
    """A readable, lossless needle representation for the browser payload."""
    if isinstance(value, str):
        raw = value.encode("utf-8")
        return {"display": value, "byte_len": len(raw), "b64": None}
    if not isinstance(value, dict) or not isinstance(value.get("b64"), str):
        raise ValueError(f"needle must be text or {{'b64': ...}}, got {value!r}")
    encoded = value["b64"]
    try:
        raw = base64.b64decode(encoded, validate=True)
    except (binascii.Error, ValueError) as error:
        raise ValueError(f"invalid base64 needle {encoded!r}: {error}") from error
    # Binary needles remain inspectable without a font or terminal silently
    # replacing bytes. Printable ASCII stays literal; everything else is \xNN.
    display = "".join(chr(byte) if 32 <= byte <= 126 else f"\\x{byte:02x}" for byte in raw)
    return {"display": display, "byte_len": len(raw), "b64": encoded}


def resolve_query_path(path: Path) -> Path:
    return path / "queries.jsonl" if path.is_dir() else path


def load_query_catalog(paths: Sequence[Path]) -> Dict[str, Dict[str, Any]]:
    """Load the query bytes and portable suite facts keyed by query id."""
    catalog: Dict[str, Dict[str, Any]] = {}
    for requested in paths:
        path = resolve_query_path(requested)
        if not path.is_file():
            raise FileNotFoundError(f"query suite not found: {path}")
        for line_number, row in enumerate(iter_rows(path), 1):
            query_id = row.get("id")
            needles = row.get("needles")
            if not isinstance(query_id, str) or not isinstance(needles, list):
                raise ValueError(f"{path}:{line_number}: expected query id and needles")
            detail = {
                "needles": [normalize_needle(value) for value in needles],
                "query_meta": row.get("meta") if isinstance(row.get("meta"), dict) else None,
            }
            previous = catalog.get(query_id)
            if previous is not None and previous != detail:
                raise ValueError(f"conflicting definitions for query id {query_id!r}")
            catalog[query_id] = detail
    return catalog


def apply_query_catalog(points: List[Dict[str, Any]],
                        catalog: Dict[str, Dict[str, Any]]) -> int:
    matched = set()
    for point in points:
        detail = catalog.get(point["query_id"])
        if detail is None:
            continue
        matched.add(point["query_id"])
        point["needles"] = detail["needles"]
        if point["query_meta"] is None:
            point["query_meta"] = detail["query_meta"]
    return len(matched)


def discover_query_paths(results: Sequence[Path], explicit: Sequence[Path]) -> List[Path]:
    """Find suite query files directly or through a benchmark manifest.

    A manifest made on another host contains absolute suite paths. When those
    point inside the repository, remap the suffix beginning at `experiments/`
    or `suites/` onto this checkout so archived runs remain useful elsewhere.
    """
    found: List[Path] = []

    def add(path: Path) -> None:
        resolved = resolve_query_path(path)
        if resolved.is_file() and resolved not in found:
            found.append(resolved)

    for path in explicit:
        resolved = resolve_query_path(path)
        if not resolved.is_file():
            raise FileNotFoundError(f"query suite not found: {resolved}")
        add(resolved)

    result_paths = []
    for requested in results:
        result_paths.extend(resolve_results_paths(requested))
        if requested.is_dir():
            for query_path in sorted(requested.rglob("queries.jsonl")):
                add(query_path)

    for result_path in result_paths:
        manifest_path = result_path.parent / "manifest.json"
        if not manifest_path.is_file():
            continue
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        for suite in manifest.get("suites") or []:
            raw = suite.get("path") if isinstance(suite, dict) else None
            if not isinstance(raw, str):
                continue
            original = Path(raw)
            add(original)
            parts = original.parts
            for marker in ("experiments", "suites"):
                if marker in parts:
                    add(REPO_ROOT.joinpath(*parts[parts.index(marker):]))
                    break
    return found


def parse_label_specs(specs: Sequence[str]) -> List[tuple]:
    """`candidate[/strategy[/scanner]]=Display Name` -> (selector, name).

    Renaming belongs here rather than in the viewer: the harness names a
    candidate after the code path it exercises (`onpair_spiral_neontable`),
    which is what you want in a result file and exactly what you do not want in
    a figure someone outside the project will read.
    """
    parsed = []
    for spec in specs:
        selector, sep, name = spec.partition("=")
        if not sep or not selector.strip() or not name.strip():
            raise ValueError(
                f"--label expects SELECTOR=NAME (e.g. 'fsst/decode=FSST'), got: {spec!r}"
            )
        parts = tuple(part.strip() for part in selector.strip().split("/"))
        if len(parts) > 3:
            raise ValueError(
                f"--label selector takes at most candidate/strategy/scanner, got: {selector!r}"
            )
        parsed.append((parts, name.strip()))
    return parsed


def compact_config(config: str) -> str:
    """Mirror of the viewer's compactConfig, so labels agree on both sides."""
    try:
        parsed = json.loads(config or "{}")
    except json.JSONDecodeError:
        return config if config and config != "{}" else ""
    if not isinstance(parsed, dict) or not parsed:
        return ""
    return ", ".join(f"{key}={value}" for key, value in parsed.items())


def apply_labels(points: List[Dict[str, Any]], specs: Sequence[str]) -> List[str]:
    """Returns the display names in declaration order, which drives the palette."""
    rules = parse_label_specs(specs)
    if not rules:
        return []

    def match(point: Dict[str, Any]) -> Optional[str]:
        keys = [
            (point["candidate"], point["strategy"], point.get("scanner") or ""),
            (point["candidate"], point["strategy"]),
            (point["candidate"],),
        ]
        for key in keys:
            for selector, name in rules:
                if selector == key:
                    return name
        return None

    resolved = [(point, match(point)) for point in points]

    # A display name that spans several configs would produce duplicate legend
    # entries, so disambiguate those -- and only those -- with the config.
    configs: Dict[str, set] = {}
    for point, name in resolved:
        if name is not None:
            configs.setdefault(name, set()).add(point["config"])

    unmatched = set()
    for point, name in resolved:
        if name is None:
            unmatched.add((point["candidate"], point["strategy"], point.get("scanner") or ""))
            continue
        if len(configs[name]) > 1:
            suffix = compact_config(point["config"])
            point["display"] = f"{name} [{suffix}]" if suffix else name
        else:
            point["display"] = name
    for candidate, strategy, scanner in sorted(unmatched):
        target = "/".join(part for part in (candidate, strategy, scanner) if part)
        print(f"bench-viz: no --label for {target} (keeping harness name)")

    ordered: List[str] = []
    for _selector, name in rules:
        if name not in ordered:
            ordered.append(name)
    return ordered


def apply_dataset_labels(points: List[Dict[str, Any]], specs: Sequence[str]) -> None:
    """`dataset=Display Name`, for the same reason as --label.

    The viewer shows one dataset at a time, so the dataset name ends up in the
    generated subtitle of every exported plot.
    """
    mapping = {}
    for spec in specs:
        name, sep, display = spec.partition("=")
        if not sep or not name.strip() or not display.strip():
            raise ValueError(
                f"--dataset-label expects NAME=DISPLAY, got: {spec!r}"
            )
        mapping[name.strip()] = display.strip()
    if not mapping:
        return
    seen = set()
    for point in points:
        seen.add(point["dataset"])
        if point["dataset"] in mapping:
            point["dataset_display"] = mapping[point["dataset"]]
    for missing in sorted(set(mapping) - seen):
        print(f"bench-viz: --dataset-label {missing!r} matched no rows")


def drop_configs(points: List[Dict[str, Any]], configs: Sequence[str]) -> List[Dict[str, Any]]:
    """Remove whole config columns by exact config string.

    The usual target is a run's noise-floor control: a candidate listed twice at
    configs that parse to the same thing. It is load-bearing when reading a run
    and pure clutter in a figure, and it cannot be renamed apart from its twin
    because a label selector deliberately does not know about configs.
    """
    if not configs:
        return points
    wanted = set(configs)
    kept = [point for point in points if point["config"] not in wanted]
    removed = len(points) - len(kept)
    if not removed:
        print(f"bench-viz: --exclude-config matched nothing: {sorted(wanted)}")
    else:
        print(f"bench-viz: dropped {removed} rows for {len(wanted)} excluded config(s)")
    if not kept:
        raise ValueError("--exclude-config removed every row")
    return kept


def normalize_build(row: Dict[str, Any], source: str) -> Optional[Dict[str, Any]]:
    """Keep the compression-axis row for its column shape.

    A query row says nothing about how many rows or payload bytes the column
    holds, and the prefilter cost model is meaningless without both: the same
    cover costs a different share of the budget on a column with five codes per
    row than on one with fifty.
    """
    if row.get("kind") != "build":
        return None
    return {
        "source": source,
        "candidate": str(row.get("candidate", "unknown")),
        "config": str(row.get("config", "{}")),
        "dataset": str(row.get("dataset", "unknown")),
        "chunk_rows": int(row.get("chunk_rows", 0) or 0),
        "num_rows": row.get("num_rows"),
        "payload_bytes": row.get("payload_bytes"),
        "raw_bytes": row.get("raw_bytes"),
        "footprint_total_bytes": row.get("footprint_total_bytes"),
        "footprint_components": row.get("footprint_components") or {},
    }


def load_results(paths: Sequence[Path]) -> tuple:
    points: List[Dict[str, Any]] = []
    builds: List[Dict[str, Any]] = []
    ignored: Dict[str, int] = {}
    resolved: List[Path] = []
    for requested in paths:
        found = resolve_results_paths(requested)
        if not found:
            raise FileNotFoundError(
                f"no results.jsonl in {requested} or in any directory directly below it"
            )
        resolved.extend(found)
    labels = source_labels(resolved)
    for path, label in zip(resolved, labels):
        if not path.is_file():
            raise FileNotFoundError(f"results file not found: {path}")
        for row in iter_rows(path):
            if row.get("kind") == "query" and not is_substring_search(row):
                op = str(row.get("op"))
                ignored[op] = ignored.get(op, 0) + 1
                continue
            point = normalize_query(row, label)
            if point is not None:
                points.append(point)
                continue
            build = normalize_build(row, label)
            if build is not None:
                builds.append(build)
    if not points:
        joined = ", ".join(str(path) for path in paths)
        if ignored:
            raise ValueError(
                f"no substring-search query rows found in: {joined} "
                f"(ignored {summarize_ignored(ignored)})"
            )
        raise ValueError(f"no successful query rows found in: {joined}")
    return points, builds, ignored


def summarize_ignored(ignored: Dict[str, int]) -> str:
    parts = ", ".join(f"{op} {count}" for op, count in sorted(ignored.items()))
    return f"{sum(ignored.values())} rows: {parts}"


def json_for_script(value: Any) -> str:
    # Prevent a query/config string containing </script> from ending the data tag.
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False).replace("<", "\\u003c")


def build_html(points: Sequence[Dict[str, Any]], defaults: Dict[str, Any],
               analysis: Optional[Dict[str, Any]] = None) -> str:
    template = (HERE / "template.html").read_text(encoding="utf-8")
    css = (HERE / "app.css").read_text(encoding="utf-8")
    javascript = (HERE / "app.js").read_text(encoding="utf-8")
    prefilter_js = (HERE / "prefilter.js").read_text(encoding="utf-8")
    replacements = {
        "__BENCH_VIZ_CSS__": css,
        "__BENCH_VIZ_DATA__": json_for_script(list(points)),
        "__BENCH_VIZ_DEFAULTS__": json_for_script(defaults),
        "__BENCH_VIZ_ANALYSIS__": json_for_script(analysis or {}),
        "__BENCH_VIZ_JS__": javascript,
        "__BENCH_VIZ_PREFILTER_JS__": prefilter_js,
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
        help="a results.jsonl, a run directory holding one, or any directory "
             "above them (every run found below it becomes a selectable Run); "
             "repeatable",
    )
    parser.add_argument(
        "--queries",
        action="append",
        default=[],
        type=Path,
        metavar="PATH",
        help="queries.jsonl or a suite directory used to add the actual needle bytes; "
             "repeatable (suite paths are also discovered from a nearby manifest)",
    )
    parser.add_argument("--out", "-o", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--title", default="Benchmark Explorer 3000™")
    parser.add_argument(
        "--subtitle",
        default=None,
        help="fixed subtitle; omit to describe whatever selection is on screen",
    )
    parser.add_argument(
        "--dataset-label",
        action="append",
        default=[],
        metavar="NAME=DISPLAY",
        help="display name for a dataset, used in the generated subtitle; repeatable",
    )
    parser.add_argument(
        "--exclude-config",
        action="append",
        default=[],
        metavar="CONFIG_JSON",
        help="drop every row whose config matches this exact string; repeatable",
    )
    parser.add_argument(
        "--label",
        action="append",
        default=[],
        metavar="SELECTOR=NAME",
        help="rename a series for display: candidate[/strategy[/scanner]]=NAME; repeatable",
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
        loaded, builds, ignored = load_results(args.results)
        if ignored:
            print("bench-viz: ignored "
                  f"{summarize_ignored(ignored)} (not substring search)")
        points = drop_configs(loaded, args.exclude_config)
        query_paths = discover_query_paths(args.results, args.queries)
        catalog = load_query_catalog(query_paths) if query_paths else {}
        enriched_queries = apply_query_catalog(points, catalog)
        if query_paths:
            query_ids = {point["query_id"] for point in points}
            missing = len(query_ids) - enriched_queries
            print(
                f"bench-viz: added needle details for {enriched_queries:,} queries "
                f"from {len(query_paths)} suite file(s)"
                + (f" ({missing:,} result queries unmatched)" if missing else "")
            )
        else:
            print("bench-viz: no query suite found; needle text will be unavailable "
                  "(pass --queries PATH to include it)")
        series_order = apply_labels(points, args.label)
        apply_dataset_labels(points, args.dataset_label)
        # Column shape, the fitted cost model and the run's own noise floor.
        # Computed here rather than in the viewer so the statistics are unit
        # testable; the viewer only reduces what is on screen.
        analysis = prefilter_model.analyse(points, builds)
        html = build_html(
            points,
            {
                "title": args.title,
                "subtitle": args.subtitle or "",
                "show": args.show,
                # Declaration order of --label, so the emphasised series gets
                # the first palette entry rather than whatever sorts first.
                "series_order": series_order,
            },
            analysis,
        )
    except (OSError, ValueError) as error:
        print(f"bench-viz: {error}")
        return 2

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(html, encoding="utf-8")
    fitted = len(analysis.get("models") or {})
    noise = analysis.get("noise")
    detail = f"{len(points)} query rows, {len(builds)} builds, {fitted} fitted series"
    if enriched_queries:
        detail += f", {enriched_queries} queries with needles"
    if noise:
        detail += f", noise floor {noise['median']:+.2%} median over {noise['pairs']} repeat pairs"
    print(f"wrote {args.out} ({detail})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
