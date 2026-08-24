"""Cost model and derived statistics for compressed-domain prefilter results.

The viewer needs three things the raw rows do not carry:

* **column shape** — codes per row and payload bytes per code. A prefilter
  streams 2 bytes per code, so these convert "codes touched" into "payload
  bytes served" and are what make throughput comparable across columns.
* **the verification load in rows** — `covered_fraction` counts code positions
  but verification is charged per row, so the two differ by codes-per-row. This
  is the variable the shipped profitability policy thresholds.
* **a predicted time** — so a result can be compared against what the cost
  model expected, and the residual read as "what the cover does not explain".

Everything here is pure Python and side-effect free so it can be unit tested
without a browser. The viewer only renders what this module computes, plus
cheap reductions (medians, win rates, sums) over whatever is on screen.

## The model

    t = codes · scan_ns_per_code(cost)  +  admitted · (rows · v_row + bytes · v_byte)

    scan_ns_per_code = sigma0 + sigma1 · cost               cover fits the specialized kernels
                     = sigma0 + gamma0 + gamma1 · cost      wider than that

    admitted = min(1, covered_codes / rows)        expected candidate rows, as a share

`cost = points + 2·ranges` is the SIMD comparisons every vector of the code
stream pays. The two scan regimes exist because kernel selection is a lattice,
not a formula: while the cover fits the specialized kernels each comparison is
cheap, and above that every shape falls through to a generic loop that costs
noticeably more per comparison. That boundary is the one library constant this
module mirrors — see `MAX_SIMD_COMPARISONS`.

The fit is per series, not pooled. Two candidates at the same revision can
plan the same cover onto different kernels, and pooling them would average two
mechanisms into one meaningless slope. Per-series fitting also means a series
whose planner escapes wide covers to a membership table simply gets its own
wide-cover constant, with no need to reconstruct its kernel-selection policy
here.
"""

from __future__ import annotations

import math
from typing import Any, Dict, Iterable, List, Optional, Sequence, Tuple

# --------------------------------------------------------------- library mirror
#
# The only constant copied out of the library: onpair's `MAX_SIMD_COMPARISONS`
# (src/search/prefilter/mod.rs), the widest cover the specialized SIMD kernels
# serve. Above it the per-comparison cost changes discontinuously, so the model
# needs a second regime and the profitability policy rejects outright. If the
# library moves it, this must move with it; nothing here can detect the drift.
MAX_SIMD_COMPARISONS = 16

# onpair's `MAX_CANDIDATE_ROW_FRACTION`: the shipped policy's verification
# budget, shown as the default gate in the decision panel.
MAX_CANDIDATE_ROW_FRACTION = 0.10

# A series needs at least this many prefilter queries before fitting it. Below
# it the two scan constants are not separable from the two verify constants and
# the fit reports nonsense with a confident-looking R².
MIN_FIT_POINTS = 24

# Joins the parts of a series identity into one string key. Must match the
# separator prefilter.js uses to look a model up, so it is named on both sides
# rather than written inline.
KEY_SEP = "\u001f"


# ------------------------------------------------------------- linear algebra


def solve_symmetric(matrix: List[List[float]], rhs: List[float]) -> Optional[List[float]]:
    """Solve `matrix · x = rhs` by Gaussian elimination with partial pivoting.

    Returns None for a singular system, which is what a series with a
    degenerate design (say, every query at the same comparison cost) produces.
    """
    n = len(rhs)
    aug = [row[:] + [rhs[i]] for i, row in enumerate(matrix)]
    for col in range(n):
        pivot = max(range(col, n), key=lambda r: abs(aug[r][col]))
        if abs(aug[pivot][col]) < 1e-12:
            return None
        aug[col], aug[pivot] = aug[pivot], aug[col]
        inv = 1.0 / aug[col][col]
        for row in range(col + 1, n):
            factor = aug[row][col] * inv
            if factor == 0.0:
                continue
            for k in range(col, n + 1):
                aug[row][k] -= factor * aug[col][k]
    out = [0.0] * n
    for col in reversed(range(n)):
        total = aug[col][n] - sum(aug[col][k] * out[k] for k in range(col + 1, n))
        out[col] = total / aug[col][col]
    return out


def least_squares(design: Sequence[Sequence[float]], target: Sequence[float],
                  weight: Sequence[float]) -> Optional[List[float]]:
    """Weighted least squares via normal equations, clamped to non-negative.

    Weights of `1/target` make this minimize *relative* error, which is what
    you want when the times being fitted span four orders of magnitude —
    otherwise the three slowest queries determine every constant.

    Clamping is not a projection onto the feasible set, it is a guard: every
    term in this model is a cost, so a negative coefficient means the design
    could not separate two terms, and zero is the honest answer.
    """
    if not design:
        return None
    width = len(design[0])
    normal = [[0.0] * width for _ in range(width)]
    rhs = [0.0] * width
    for row, y, w in zip(design, target, weight):
        ww = w * w
        for i in range(width):
            if row[i] == 0.0:
                continue
            rhs[i] += ww * row[i] * y
            for j in range(width):
                if row[j] != 0.0:
                    normal[i][j] += ww * row[i] * row[j]
    # Two kinds of degeneracy are normal here, and neither should be an error.
    #
    # A term with no observations -- a series whose covers never leave the
    # specialized kernels has no wide-cover column -- contributes an all-zero
    # column. And on a run covering a single column, `admitted * rows` and
    # `admitted * bytes` are exact multiples of each other, because bytes is
    # rows times a constant, so the per-row and per-byte verify costs are not
    # separable at all. Only a run spanning columns of different row length can
    # tell them apart.
    #
    # In both cases the honest response is to fit fewer constants rather than
    # to invent one, so terms are dropped from the right until the system
    # solves. The surviving terms absorb what was dropped, which keeps the
    # prediction exact and only costs interpretability of the individual
    # constants.
    live = [i for i in range(width) if normal[i][i] > 0.0]
    while live:
        reduced = [[normal[i][j] for j in live] for i in live]
        solved = solve_symmetric(reduced, [rhs[i] for i in live])
        if solved is not None:
            out = [0.0] * width
            for slot, value in zip(live, solved):
                out[slot] = max(0.0, value)
            return out
        live.pop()
    return None


# ------------------------------------------------------------- column shape


def column_key(point: Dict[str, Any]) -> Tuple[str, str, int]:
    """Payload bytes and row count are dataset properties, not candidate ones."""
    return (point.get("source") or "", point.get("dataset") or "", int(point.get("chunk_rows") or 0))


def series_key(point: Dict[str, Any]) -> Tuple[str, ...]:
    return (
        point.get("source") or "",
        point.get("candidate") or "",
        point.get("config") or "",
        point.get("strategy") or "",
        point.get("scanner") or "",
    )


def series_key_any_source(point: Dict[str, Any]) -> Tuple[str, ...]:
    """The same identity with the run dropped.

    Used only as a fallback when looking a model up. A single-predicate run
    holds one query and can never be fitted, but it is the same code on the
    same machine as the sweep beside it, so borrowing the sweep's constants is
    right where refusing to predict is merely unhelpful. Keying the fit itself
    by source keeps two runs from different machines out of one fit.
    """
    return series_key(point)[1:]


def column_facts(builds: Iterable[Dict[str, Any]], points: Sequence[Dict[str, Any]]
                 ) -> Dict[Tuple[str, str, int], Dict[str, Any]]:
    """Rows, payload bytes and code count per column, from builds where the run
    reports them and from the query rows where it does not.

    Older runs predate `num_rows`/`payload_bytes` on the build record, but both
    are exactly recoverable from a query row: `gbps_raw` is payload bytes per
    nanosecond by construction, and `ns_per_row` is the median divided by the
    row count. Recovering them keeps this tool usable on results already on
    disk rather than only on runs made after the harness change.
    """
    facts: Dict[Tuple[str, str, int], Dict[str, Any]] = {}
    for build in builds:
        key = (build.get("source") or "", str(build.get("dataset") or ""),
               int(build.get("chunk_rows") or 0))
        entry = facts.setdefault(key, {})
        for name in ("num_rows", "payload_bytes"):
            value = build.get(name)
            if isinstance(value, (int, float)) and value > 0:
                entry[name] = float(value)
        codes = (build.get("footprint_components") or {}).get("codes")
        if isinstance(codes, (int, float)) and codes > 0:
            # u16 codes on disk; the model counts positions, not bytes.
            entry.setdefault("build_codes", float(codes) / 2.0)
            entry.setdefault("code_bytes", float(codes))
        # `payload_bytes` is the concatenated strings with no offset array, so
        # the comparable compressed size excludes the row offsets too -- and the
        # prefilter's frequency index, which is a search structure the payload
        # has no counterpart for. The breakdown is carried through so the viewer
        # can say what it left out rather than just asserting a number.
        components = build.get("footprint_components") or {}
        sized = {name: float(value) for name, value in components.items()
                 if isinstance(value, (int, float)) and value > 0}
        if sized:
            entry.setdefault("footprint_components", sized)
            excluded = ("row_offsets", "prefilter")
            entry.setdefault("compressed_bytes", sum(
                value for name, value in sized.items() if name not in excluded))
            entry.setdefault("compressed_excludes", [
                name for name in excluded if name in sized])
        total = build.get("footprint_total_bytes")
        if isinstance(total, (int, float)) and total > 0:
            entry.setdefault("footprint_total_bytes", float(total))

    for point in points:
        entry = facts.setdefault(column_key(point), {})
        latency = point.get("latency_ns")
        if not (isinstance(latency, (int, float)) and latency > 0):
            continue
        if "payload_bytes" not in entry and isinstance(point.get("gbps"), (int, float)):
            # gbps_raw == payload_bytes / median_ns numerically.
            entry["payload_bytes"] = float(point["gbps"]) * float(latency)
        if "num_rows" not in entry:
            per_row = point.get("ns_per_row")
            if isinstance(per_row, (int, float)) and per_row > 0:
                entry["num_rows"] = float(latency) / float(per_row)

    for entry in facts.values():
        rows = entry.get("num_rows")
        payload = entry.get("payload_bytes")
        if rows:
            entry["bytes_per_row"] = (payload / rows) if payload else None
    return facts


def _codes_for(point: Dict[str, Any], column: Dict[str, Any]) -> Optional[float]:
    """Code positions the cover was analysed over, per query where reported."""
    indexed = point.get("indexed_codes")
    if isinstance(indexed, (int, float)) and indexed > 0:
        return float(indexed)
    build_codes = column.get("build_codes")
    return float(build_codes) if build_codes else None


def annotate_shape(points: Sequence[Dict[str, Any]],
                   facts: Dict[Tuple[str, str, int], Dict[str, Any]]) -> None:
    """Attach per-point column shape and the verification load in rows.

    Adds `codes`, `codes_per_row`, `bytes_per_code`, `kappa` (payload bytes per
    byte of code stream, the compression ratio the prefilter actually enjoys),
    `admitted_rows` (the share of rows the cover sends to verification) and
    `arm`.
    """
    for point in points:
        column = facts.get(column_key(point)) or {}
        rows = column.get("num_rows")
        payload = column.get("payload_bytes")
        codes = _codes_for(point, column)
        point["codes"] = codes
        point["column_rows"] = rows
        point["column_bytes"] = payload
        point["codes_per_row"] = (codes / rows) if (codes and rows) else None
        point["bytes_per_code"] = (payload / codes) if (codes and payload) else None
        point["kappa"] = (payload / (2.0 * codes)) if (codes and payload) else None

        admitted = point.get("candidate_row_fraction")
        if not isinstance(admitted, (int, float)):
            # Recover it the way the harness now computes it, or failing that
            # from the coverage ratio and the column's codes-per-row.
            covered = point.get("covered_codes")
            if isinstance(covered, (int, float)) and rows:
                admitted = min(1.0, float(covered) / rows)
            elif isinstance(point.get("covered_fraction"), (int, float)) and point["codes_per_row"]:
                admitted = min(1.0, point["covered_fraction"] * point["codes_per_row"])
            else:
                admitted = None
        point["admitted_rows"] = admitted

        cost = point.get("comparison_cost")
        point["arm"] = None if not isinstance(cost, (int, float)) else (
            "specialized" if cost <= MAX_SIMD_COMPARISONS else "wide")


# ------------------------------------------------------------------- the fit

TERMS = ("sigma0", "sigma1", "gamma0", "gamma1", "v_row", "v_byte")


def _fittable(point: Dict[str, Any]) -> bool:
    return (
        isinstance(point.get("comparison_cost"), (int, float))
        and isinstance(point.get("admitted_rows"), (int, float))
        and isinstance(point.get("latency_ns"), (int, float))
        and point["latency_ns"] > 0
        and bool(point.get("codes"))
        and bool(point.get("column_rows"))
        and bool(point.get("column_bytes"))
    )


def _design_row(point: Dict[str, Any]) -> List[float]:
    codes = float(point["codes"])
    cost = float(point["comparison_cost"])
    admitted = float(point["admitted_rows"])
    specialized = point["arm"] == "specialized"
    return [
        codes,                                       # sigma0, both regimes
        codes * cost if specialized else 0.0,        # sigma1
        0.0 if specialized else codes,               # gamma0, wide-regime step
        0.0 if specialized else codes * cost,        # gamma1
        admitted * float(point["column_rows"]),      # v_row
        admitted * float(point["column_bytes"]),     # v_byte
    ]


def fit_cost_model(points: Sequence[Dict[str, Any]]) -> Dict[str, Any]:
    """Fit one set of constants per series and report how well each does.

    Returns `{series_key_string: {constants, diagnostics}}` plus a pooled
    summary. Series without enough fittable queries are absent, and their
    points simply carry no prediction.
    """
    by_series: Dict[Tuple[str, ...], List[Dict[str, Any]]] = {}
    for point in points:
        if _fittable(point):
            by_series.setdefault(series_key(point), []).append(point)

    models: Dict[str, Any] = {}
    borrowable: Dict[Tuple[str, ...], str] = {}
    for key, mine in by_series.items():
        if len(mine) < MIN_FIT_POINTS:
            continue
        design = [_design_row(p) for p in mine]
        target = [float(p["latency_ns"]) for p in mine]
        weight = [1.0 / t for t in target]
        beta = least_squares(design, target, weight)
        if beta is None:
            continue
        errors = []
        for point, row in zip(mine, design):
            predicted = sum(b * r for b, r in zip(beta, row))
            if predicted > 0:
                errors.append(abs(math.log(predicted / float(point["latency_ns"]))))
        errors.sort()
        name = KEY_SEP.join(key)
        # Prefer the largest fit when several runs measured the same series.
        sibling = key[1:]
        previous = borrowable.get(sibling)
        if previous is None or models[previous]["n"] < len(mine):
            borrowable[sibling] = name
        models[name] = {
            "constants": dict(zip(TERMS, beta)),
            "n": len(mine),
            "source": key[0],
            "median_abs_log_error": errors[len(errors) // 2] if errors else None,
            "p90_abs_log_error": errors[min(len(errors) - 1, int(0.9 * len(errors)))] if errors else None,
            # 2 bytes per code position: the streaming ceiling the scan floor implies.
            "code_stream_gbps": (2.0 / beta[0]) if beta[0] > 0 else None,
        }
    # Aliases so a run too small to fit still resolves to the same series.
    for sibling, name in borrowable.items():
        alias = KEY_SEP.join(("", *sibling))
        if alias not in models:
            models[alias] = dict(models[name], borrowed_from=models[name]["source"])
    return models


def annotate_predictions(points: Sequence[Dict[str, Any]], models: Dict[str, Any]) -> None:
    """Attach `predicted_ns` and its two components.

    The split is worth keeping: a query the model gets wrong because the scan
    term is off is a kernel question, and one it gets wrong because the verify
    term is off is a question about how many rows the cover really admitted.
    Both are invisible in a single predicted total.
    """
    for point in points:
        point["predicted_ns"] = None
        point["predicted_scan_ns"] = None
        point["predicted_verify_ns"] = None
        if not _fittable(point):
            continue
        model = models.get(KEY_SEP.join(series_key(point)))
        if model is None:
            model = models.get(KEY_SEP.join(("", *series_key_any_source(point))))
        if model is None:
            continue
        constants = model["constants"]
        codes = float(point["codes"])
        cost = float(point["comparison_cost"])
        per_code = constants["sigma0"] + (
            constants["sigma1"] * cost if point["arm"] == "specialized"
            else constants["gamma0"] + constants["gamma1"] * cost
        )
        scan = codes * per_code
        verify = float(point["admitted_rows"]) * (
            float(point["column_rows"]) * constants["v_row"]
            + float(point["column_bytes"]) * constants["v_byte"]
        )
        point["predicted_scan_ns"] = scan
        point["predicted_verify_ns"] = verify
        point["predicted_ns"] = scan + verify


# ------------------------------------------------------------- noise floor


def twin_configs(builds: Iterable[Dict[str, Any]]
                 ) -> Dict[Tuple[str, str, str, int], List[str]]:
    """Config strings of one candidate that built a byte-identical column.

    Two configs that parse to the same settings still hash differently, because
    the harness hashes the config *string*. So they cannot be recognised by
    their key. What they do share is the column they produced: identical
    footprint components mean the same dictionary and the same code stream,
    which means two worker processes doing the same work.

    That is a strong signal rather than a proof. A config that changed kernel
    selection without changing the column would look like a twin and its
    difference would be counted as noise, so the matched configs are reported
    for the reader to check rather than only their conclusion.
    """
    grouped: Dict[Tuple[str, str, str, int], Dict[str, List[str]]] = {}
    for build in builds:
        components = build.get("footprint_components") or {}
        if not components:
            continue
        signature = "|".join(
            f"{name}={components[name]}" for name in sorted(components)
        ) + f"|total={build.get('footprint_total_bytes')}"
        key = (build.get("source") or "", str(build.get("candidate") or ""),
               str(build.get("dataset") or ""), int(build.get("chunk_rows") or 0))
        grouped.setdefault(key, {}).setdefault(signature, []).append(
            str(build.get("config") or ""))
    out: Dict[Tuple[str, str, str, int], List[str]] = {}
    for key, signatures in grouped.items():
        for configs in signatures.values():
            unique = sorted(set(configs))
            if len(unique) > 1:
                out[key] = unique
    return out


def noise_floor(points: Sequence[Dict[str, Any]],
                builds: Sequence[Dict[str, Any]]) -> Optional[Dict[str, Any]]:
    """Repeat measurements of identical work, found automatically.

    A spec that lists one candidate twice at configs that parse identically
    makes two worker processes do the same work, and the spread between them is
    this run's own resolution. Without it a viewer invites reading a 3%
    difference as a result.

    Returns None when a run has no such pair, which is a fact worth surfacing
    rather than papering over.
    """
    twins = twin_configs(builds)
    if not twins:
        return None
    grouped: Dict[Tuple[str, ...], Dict[str, float]] = {}
    for point in points:
        latency = point.get("latency_ns")
        if not (isinstance(latency, (int, float)) and latency > 0):
            continue
        column = (point.get("source") or "", str(point.get("candidate") or ""),
                  str(point.get("dataset") or ""), int(point.get("chunk_rows") or 0))
        configs = twins.get(column)
        if not configs or (point.get("config") or "") not in configs:
            continue
        key = (*column, point.get("strategy") or "", point.get("scanner") or "",
               point.get("query_id") or "")
        grouped.setdefault(key, {})[point.get("config") or ""] = float(latency)

    ratios: List[float] = []
    for variants in grouped.values():
        if len(variants) < 2:
            continue
        times = sorted(variants.values())
        # One pair per group: the extremes bound the spread of identical work.
        if times[0] > 0:
            ratios.append(times[-1] / times[0])
    if not ratios:
        return None
    ratios.sort()
    deviations = [r - 1.0 for r in ratios]
    return {
        "pairs": len(ratios),
        "median": deviations[len(deviations) // 2],
        "p90": deviations[min(len(deviations) - 1, int(0.9 * len(deviations)))],
        "max": deviations[-1],
        "matched": [
            {"candidate": key[1], "dataset": key[2], "configs": configs}
            for key, configs in sorted(twins.items())
        ],
    }


# ------------------------------------------------------------------ assembly


def analyse(points: Sequence[Dict[str, Any]], builds: Sequence[Dict[str, Any]]) -> Dict[str, Any]:
    """Annotate `points` in place and return everything the viewer needs.

    Order matters: shape before the fit, because the fit reads `arm` and
    `admitted_rows`; the fit before predictions, obviously.
    """
    facts = column_facts(builds, points)
    annotate_shape(points, facts)
    models = fit_cost_model(points)
    annotate_predictions(points, models)
    return {
        "columns": {
            KEY_SEP.join((key[0], key[1], str(key[2]))): value
            for key, value in facts.items()
        },
        "models": models,
        "noise": noise_floor(points, builds),
        "max_simd_comparisons": MAX_SIMD_COMPARISONS,
        "max_candidate_row_fraction": MAX_CANDIDATE_ROW_FRACTION,
    }
