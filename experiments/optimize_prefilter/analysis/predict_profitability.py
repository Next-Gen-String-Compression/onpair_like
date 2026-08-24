#!/usr/bin/env python3
"""Predict prefilter throughput, and decide profitability, from pre-scan facts.

The reference implementation behind the figures pinned in
tools/bench-viz/tests/figures.test.js: that test recomputes the same statistics
in JavaScript and checks they agree. Two implementations of the same reductions,
in different languages, written against the same data, is stronger evidence than
either passing its own unit tests.

    python3 experiments/optimize_prefilter/analysis/predict_profitability.py

Not part of any default test run: it needs numpy and two measurement runs that
are build products, so it is something you set up deliberately.

The series names below key into the runs it reads, not into this repo's
candidate list: the campaign compared several kernel variants that live in a
separate lab checkout. Only `onpair_spiral` is a candidate here.

The subject is `onpair_spiral` -- upstream 6408585, whose `select_aarch64` has no
row-table arm, so every cover runs the SIMD comparison kernels. That is the path
worth modelling: the row table is a fallback we do not want to depend on, and
excluding it makes the profitability gate load-bearing rather than cosmetic.
`onpair_spiral_neontable` is the same revision plus the NEON row-table escape,
and appears only in the section that prices what the table would change.

Requires numpy. Reads:
    results/optimize_prefilter/figures/needle-sweep   (2876 needles, 3 columns)
    results/optimize_prefilter/figures/like-google    (single predicate)
Both carry ABI v6 cover facts in band -- comparison_cost, covered_fraction,
cover_points, cover_ranges, profitable_hint -- so the join needs no side census.
"""

from __future__ import annotations

import collections
import json
import math
import statistics as st
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[3]
RUNS = [
    ROOT / "results/optimize_prefilter/figures/needle-sweep",
    ROOT / "results/optimize_prefilter/figures/like-google",
]

SERIES = {
    # The subject: SIMD comparison kernels for every cover.
    ("onpair_spiral", '{"bits":16}', "pf_memmem", None): "simd",
    # Same revision plus the NEON row-table escape. Section 7 only.
    ("onpair_spiral_neontable", '{"bits":16}', "pf_memmem", None): "table",
    # Same config after parsing: the run's own noise control.
    ("onpair_spiral_neontable", '{"bits":16,"threshold":0.15}', "pf_memmem", None): "twin",
    ("onpair_spiral_decode", '{"bits":16}', "decode", "memmem-hay"): "dec",
    ("uncompressed_memmem", "{}", "memmem-hay", None): "unc",
    ("fsst", "{}", "decode", "memmem-hay"): "fsst",
}

SHORT = {"clickbench-url-1m": "clickbench", "amazon-title": "amazon",
         "dbpedia-abstract": "dbpedia"}

# The specialized-kernel ceiling, from prefilter/mod.rs and select_neon: above
# 16 comparisons every cover shape falls through to Kernel::Generic.
MAX_SIMD_COMPARISONS = 16


def load():
    rows: dict[tuple[str, str], dict] = collections.defaultdict(dict)
    builds: dict[str, dict] = {}
    for run in RUNS:
        with (run / "results.jsonl").open() as handle:
            for line in handle:
                rec = json.loads(line)
                if rec.get("kind") == "build":
                    if rec["candidate"] == "onpair_spiral" and rec["config"] == '{"bits":16}':
                        builds[rec["dataset"]] = rec
                    continue
                if rec.get("kind") != "query":
                    continue
                name = SERIES.get(
                    (rec["candidate"], rec["config"], rec["strategy"], rec.get("scanner"))
                )
                if name is None:
                    continue
                # Every cell in these runs was gate-checked against the oracle.
                assert rec["status"] == "ok" and rec["gate"]["hash_ok"], rec["query_id"]
                cell = rows[(rec["dataset"], rec["query_id"])]
                pf = rec.get("prefilter") or {}
                cell[name] = dict(ns=float(rec["latency"]["median_ns"]),
                                  decode_ns=(pf.get("decode_ns") or {}).get("ns"))
                if name in ("simd", "table", "twin"):
                    cell[name].update(
                        cost=float(pf["comparison_cost"]), cov=pf["covered_fraction"],
                        points=pf["cover_points"], ranges=pf["cover_ranges"],
                        hint=bool(pf["profitable_hint"]),
                    )
                cell["len"] = rec["derived"]["needle_len_total"]
                cell["sel"] = rec["derived"]["selectivity"]

    cols = {}
    for ds, build in builds.items():
        payload = json.loads((ROOT / "datasets" / ds / "manifest.json").read_text())
        codes = build["footprint_components"]["codes"] // 2   # u16 codes
        cols[ds] = dict(
            short=SHORT[ds], B=float(payload["payload_bytes"]),
            nrow=float(payload["num_rows"]), N=float(codes),
            k=codes / payload["num_rows"],
            bytes_per_code=payload["payload_bytes"] / codes,
            kappa=payload["payload_bytes"] / (2 * codes),
            bytes_per_row=payload["payload_bytes"] / payload["num_rows"],
        )

    out = []
    for (ds, qid), cell in rows.items():
        if "simd" not in cell or "dec" not in cell:
            continue
        col = cols[ds]
        pf = cell["simd"]
        out.append(dict(
            ds=ds, short=col["short"], qid=qid, ln=cell["len"], sel=cell["sel"],
            t=pf["ns"], cost=pf["cost"], cov=pf["cov"],
            points=pf["points"], ranges=pf["ranges"], hint=pf["hint"],
            # Expected candidate rows per row. Exact for single-token covers, an
            # upper bound otherwise, capped because a row is admitted once.
            a=min(1.0, col["k"] * pf["cov"]),
            t_table=cell.get("table", {}).get("ns"), t_twin=cell.get("twin", {}).get("ns"),
            t_dec=cell["dec"]["ns"], t_dec_only=float(cell["dec"]["decode_ns"]),
            t_unc=cell["unc"]["ns"], t_fsst=cell["fsst"]["ns"],
            arm="spec" if pf["cost"] <= MAX_SIMD_COMPARISONS else "gen",
            **{k: col[k] for k in ("N", "nrow", "B", "k", "bytes_per_code",
                                   "kappa", "bytes_per_row")},
        ))

    # Column-constant baselines: what a planner knows before it sees the needle.
    for ds, col in cols.items():
        mine = [r for r in out if r["ds"] == ds]
        col["T_base"] = st.median([r["t_dec"] for r in mine])
        col["T_decode"] = st.median([r["t_dec_only"] for r in mine])
        col["T_unc"] = st.median([r["t_unc"] for r in mine])
    for r in out:
        col = cols[r["ds"]]
        r["T_base"], r["T_decode"], r["T_unc"] = col["T_base"], col["T_decode"], col["T_unc"]
    return out, cols


NAMES = ["sigma0  ns/code, specialized floor", "sigma1  ns/code per comparison",
         "gamma0  ns/code, generic floor", "gamma1  ns/code per comparison, generic",
         "v_row   ns per admitted row", "v_byte  ns per admitted byte"]


def design(rows) -> np.ndarray:
    get = lambda key: np.array([r[key] for r in rows], dtype=float)
    N, cost, a, nrow, B = get("N"), get("cost"), get("a"), get("nrow"), get("B")
    spec = np.array([r["arm"] == "spec" for r in rows], dtype=float)
    gen = 1.0 - spec
    return np.column_stack([N * spec, N * cost * spec, N * gen, N * cost * gen,
                            a * nrow, a * B])


def fit(rows) -> np.ndarray:
    X, y = design(rows), np.array([r["t"] for r in rows])
    weight = 1.0 / y                      # minimize relative, not absolute, error
    beta, *_ = np.linalg.lstsq(X * weight[:, None], y * weight, rcond=None)
    return np.maximum(beta, 0.0)          # every term is a cost


def accuracy(rows, beta):
    ratio = (design(rows) @ beta) / np.array([r["t"] for r in rows])
    err = np.abs(np.log(ratio))
    return np.median(err), np.percentile(err, 90), float(np.median(ratio))


def main() -> None:
    np.seterr(all="ignore")
    rows, cols = load()
    print(f"{len(rows)} queries, {len(cols)} columns, all gate-checked")
    twin = [abs(r["t_table"] / r["t_twin"] - 1) for r in rows if r["t_twin"] and r["t_table"]]
    print(f"noise floor (twin configs parsing to the same Config): n={len(twin)} "
          f"median |ratio-1| {st.median(twin):.2%}  p90 {np.percentile(twin, 90):.2%}\n")

    print("== 1. the baseline that matters ==")
    print(f"  {'column':11s} {'MB':>7s} {'B/code':>7s} {'kappa':>6s} {'codes/row':>9s} "
          f"{'decode only':>13s} {'+memmem':>13s} {'uncompressed':>14s}")
    for col in cols.values():
        gb = lambda t: col["B"] / t
        print(f"  {col['short']:11s} {col['B'] / 1e6:7.1f} {col['bytes_per_code']:7.2f} "
              f"{col['kappa']:6.2f} {col['k']:9.2f} "
              f"{col['T_decode'] / 1e6:8.2f}ms {gb(col['T_decode']):4.1f} "
              f"{col['T_base'] / 1e6:8.2f}ms {gb(col['T_base']):4.1f} "
              f"{col['T_unc'] / 1e6:9.2f}ms {gb(col['T_unc']):4.1f}")
    print("\n  share of needles where the SIMD prefilter is faster:")
    for col in list(cols.values()) + [None]:
        mine = rows if col is None else [r for r in rows if r["short"] == col["short"]]
        share = lambda key: sum(1 for r in mine if r["t"] < r[key]) / len(mine)
        label = "pooled" if col is None else col["short"]
        print(f"    {label:11s} vs decode+memmem {share('t_dec'):6.1%}   "
              f"vs decode only {share('t_dec_only'):6.1%}   "
              f"vs uncompressed {share('t_unc'):6.1%}")

    print("\n== 2. the model ==")
    beta = fit(rows)
    for name, value in zip(NAMES, beta):
        print(f"  {name:38s} {value:.6f}")
    s0, s1, g0, g1, v_row, v_byte = beta
    med, p90, bias = accuracy(rows, beta)
    print(f"\n  all {len(rows)}: median |error| {math.exp(med) - 1:+.1%}  "
          f"p90 {math.exp(p90) - 1:+.1%}  bias {bias:.3f}x")
    for col in cols.values():
        med, p90, bias = accuracy([r for r in rows if r["short"] == col["short"]], beta)
        print(f"    {col['short']:11s} median {math.exp(med) - 1:+.1%}  "
              f"p90 {math.exp(p90) - 1:+.1%}  bias {bias:.3f}x")
    print("  leave-one-column-out (constants fitted on two columns, applied to the third):")
    for col in cols.values():
        held = [r for r in rows if r["short"] == col["short"]]
        med, p90, bias = accuracy(held, fit([r for r in rows if r["short"] != col["short"]]))
        print(f"    {col['short']:11s} median {math.exp(med) - 1:+.1%}  "
              f"p90 {math.exp(p90) - 1:+.1%}  bias {bias:.3f}x")
    for arm in ("spec", "gen"):
        mine = [r for r in rows if r["arm"] == arm]
        med, p90, bias = accuracy(mine, beta)
        print(f"    arm {arm:5s} n={len(mine):4d} median {math.exp(med) - 1:+.1%}  "
              f"p90 {math.exp(p90) - 1:+.1%}  bias {bias:.3f}x")

    truth = np.log(np.array([r["B"] / r["t"] for r in rows]))
    guess = np.log(np.array([r["B"] for r in rows]) / (design(rows) @ beta))
    print(f"\n  R^2 on log(GB/s): pooled {1 - np.var(truth - guess) / np.var(truth):.4f}", end="")
    for col in cols.values():
        idx = [i for i, r in enumerate(rows) if r["short"] == col["short"]]
        print(f"   {col['short']} "
              f"{1 - np.var(truth[idx] - guess[idx]) / np.var(truth[idx]):.4f}", end="")
    print("\n  contrast, same target, plain log-linear fits:")
    log = lambda key: np.log(np.maximum(np.array([r[key] for r in rows], dtype=float), 1e-12))
    for label, feats in (("log selectivity", [log("sel")]),
                         ("log comparison cost", [log("cost")]),
                         ("log covered_fraction", [log("cov")]),
                         ("log cost + log coverage", [log("cost"), log("cov")])):
        X = np.column_stack([np.ones(len(rows))] + feats)
        coef, *_ = np.linalg.lstsq(X, truth, rcond=None)
        print(f"    {label:26s} {1 - np.var(truth - X @ coef) / np.var(truth):.4f}")
    print(f"\n  measured scan rate by cost, low-admission queries only (a < 0.02):")
    by = collections.defaultdict(list)
    for r in rows:
        if r["a"] < 0.02:
            by[int(r["cost"])].append(r["t"] / r["N"])
    print(f"    {'cost':>8s} {'n':>5s} {'min ns/code':>12s} {'median':>9s} {'model':>9s}")
    for lo, hi in [(1, 1), (2, 2), (3, 4), (5, 8), (9, 12), (13, 16),
                   (17, 24), (25, 40), (41, 64), (65, 128), (129, 1 << 30)]:
        seen = [v for c, vs in by.items() if lo <= c <= hi for v in vs]
        if not seen:
            continue
        mid = (lo + min(hi, 128)) / 2
        model = s0 + s1 * mid if mid <= 16 else g0 + g1 * mid
        label = f"{lo}-{hi}" if hi < 1 << 30 else f"{lo}+"
        print(f"    {label:>8s} {len(seen):5d} {min(seen):12.4f} {st.median(seen):9.4f} {model:9.4f}")

    print("\n== 3. the rule, in dimensionless form ==")
    W = 2.0 / s0
    print(f"  ns/code ~= sigma0*(1+cost), since sigma1/sigma0 = {s1 / s0:.2f}")
    print(f"  code-stream bandwidth W = 2/sigma0 = {W:.0f} GB/s")
    print(f"  generic arm costs gamma1/sigma1 = {g1 / s1:.1f}x more per comparison")
    print("\n    u  =  (1 + cost) / Smax   +   phi * a        prefilter iff cost <= 16 and u < 1")
    print("    Smax = kappa * W / g_baseline      ceiling speedup, a column constant")
    print("    phi  = verify ns per row / bulk ns per row     also a column constant")
    print(f"\n  {'column':11s} {'kappa':>6s} {'g_base':>7s} {'Smax':>7s} {'phi':>6s} "
          f"{'a_max @cost1':>13s} {'a_max @cost16':>14s}")
    for col in cols.values():
        col["Smax"] = col["kappa"] * W / (col["B"] / col["T_base"])
        col["phi"] = (v_row + col["bytes_per_row"] * v_byte) / (col["T_base"] / col["nrow"])
        print(f"  {col['short']:11s} {col['kappa']:6.2f} {col['B'] / col['T_base']:7.1f} "
              f"{col['Smax']:6.1f}x {col['phi']:6.2f} "
              f"{(1 - 2 / col['Smax']) / col['phi']:13.3f} "
              f"{(1 - 17 / col['Smax']) / col['phi']:14.3f}")
    for r in rows:
        col = cols[r["ds"]]
        r["u"] = (1 + r["cost"]) / col["Smax"] + col["phi"] * r["a"]
        r["gate"] = r["cost"] <= MAX_SIMD_COMPARISONS and r["u"] < 1.0
        r["win"] = r["t"] < r["t_dec"]

    print("\n== 4. does u track the outcome? (specialized arm only) ==")
    spec = [r for r in rows if r["arm"] == "spec"]
    print(f"  {'u band':>10s} {'n':>5s} {'win rate':>9s} {'median speedup':>15s} {'median 1/u':>11s}")
    edges = [0, .05, .1, .2, .35, .5, .7, .9, 1.0, 1.3, 2.0, 5.0, float("inf")]
    for lo, hi in zip(edges, edges[1:]):
        mine = [r for r in spec if lo <= r["u"] < hi]
        if not mine:
            continue
        label = f"{lo:g}-{hi:g}" if hi != float("inf") else f"{lo:g}+"
        speed = sorted(r["t_dec"] / r["t"] for r in mine)
        print(f"  {label:>10s} {len(mine):5d} {sum(r['win'] for r in mine) / len(mine):8.1%} "
              f"{st.median(speed):14.2f}x {st.median([1 / r['u'] for r in mine]):10.2f}x")
    x = np.log([1 / r["u"] for r in spec])
    y = np.log([r["t_dec"] / r["t"] for r in spec])
    A = np.column_stack([np.ones(len(x)), x])
    coef, *_ = np.linalg.lstsq(A, y, rcond=None)
    ratio = [(r["t_dec"] / r["t"]) / (1 / r["u"]) for r in spec]
    print(f"  1/u as a speedup estimate: R^2 {1 - np.var(y - A @ coef) / np.var(y):.4f}  "
          f"slope {coef[1]:.2f}  median predicted/observed {st.median(ratio):.3f}  "
          f"p10 {np.percentile(ratio, 10):.2f} p90 {np.percentile(ratio, 90):.2f}")

    print("\n== 5. gates, scored on total time actually spent ==")

    def rule(name, gate, baseline="t_dec"):
        spent = sum(r["t"] if gate(r) else r[baseline] for r in rows)
        oracle = sum(min(r["t"], r[baseline]) for r in rows)
        fp = [r for r in rows if gate(r) and r["t"] >= r[baseline]]
        fn = sum(1 for r in rows if not gate(r) and r["t"] < r[baseline])
        worst = max([r["t"] / r[baseline] for r in fp] + [1.0])
        print(f"  {name:44s} FP {len(fp):3d} FN {fn:4d}  regret {spent / oracle:5.3f}x  "
              f"worst regression {worst:5.2f}x")

    rule("always prefilter", lambda r: True)
    rule("shipped: len>=4 & cost<=16 & cov<1%", lambda r: r["hint"])
    rule("cost<=16, no coverage clause", lambda r: r["arm"] == "spec")
    for thresh in (0.06, 0.10, 0.14, 0.20):
        rule(f"cost<=16 & k*cov<{thresh:.2f}",
             lambda r, t=thresh: r["arm"] == "spec" and r["a"] < t)
    rule("cost<=16 & u<1  (recommended)", lambda r: r["gate"])
    rule("oracle", lambda r: r["t"] < r["t_dec"])
    print("  against decode only, the harder floor:")
    for col in cols.values():
        col["Smax_d"] = col["kappa"] * W / (col["B"] / col["T_decode"])
        col["phi_d"] = (v_row + col["bytes_per_row"] * v_byte) / (col["T_decode"] / col["nrow"])
    for r in rows:
        col = cols[r["ds"]]
        r["u_d"] = (1 + r["cost"]) / col["Smax_d"] + col["phi_d"] * r["a"]
    rule("shipped hint", lambda r: r["hint"], "t_dec_only")
    rule("cost<=16 & u_decode<1", lambda r: r["arm"] == "spec" and r["u_d"] < 1, "t_dec_only")
    rule("oracle", lambda r: r["t"] < r["t_dec_only"], "t_dec_only")

    print("\n== 6. what the recommended gate keeps, and what cost>16 would cost ==")
    passed = [r for r in rows if r["gate"]]
    speed = sorted(r["t_dec"] / r["t"] for r in passed)
    gbps = sorted(r["B"] / r["t"] for r in passed)
    print(f"  passes {len(passed)}/{len(rows)} ({len(passed) / len(rows):.1%}); "
          f"every one of them beats decode+memmem: {all(r['win'] for r in passed)}")
    print(f"  median speedup {st.median(speed):.2f}x  p10 {np.percentile(speed, 10):.2f}x  "
          f"p90 {np.percentile(speed, 90):.2f}x")
    print(f"  logical throughput median {st.median(gbps):.0f} GB/s  "
          f"p90 {np.percentile(gbps, 90):.0f}  max {gbps[-1]:.0f}")
    missed = [r for r in rows if not r["gate"] and r["win"]]
    print(f"  declines {len(rows) - len(passed)}, of which {len(missed)} would have won "
          f"(median {st.median([r['t_dec'] / r['t'] for r in missed]):.2f}x, "
          f"max {max(r['t_dec'] / r['t'] for r in missed):.2f}x)")
    print(f"  {'needle len':>12s} {'n':>5s} {'pass rate':>10s} {'median speedup when passed':>27s}")
    for label, lo, hi in [("1", 1, 1), ("2", 2, 2), ("3-4", 3, 4), ("5-8", 5, 8),
                          ("9-16", 9, 16), ("17-32", 17, 32), ("33-64", 33, 64)]:
        mine = [r for r in rows if lo <= r["ln"] <= hi]
        ok = [r for r in mine if r["gate"]]
        med = st.median([r["t_dec"] / r["t"] for r in ok]) if ok else float("nan")
        print(f"  {label:>12s} {len(mine):5d} {len(ok) / len(mine):9.1%} {med:26.2f}x")
    wide = [r for r in rows if r["arm"] == "gen"]
    win = [r for r in wide if r["win"]]
    lose = [r for r in wide if not r["win"]]
    print(f"\n  cost>16: {len(wide)} queries ({len(wide) / len(rows):.1%}). "
          f"Running all: {sum(r['t'] for r in wide) / 1e9:.2f}s vs "
          f"{sum(r['t_dec'] for r in wide) / 1e9:.2f}s decoding "
          f"({sum(r['t'] for r in wide) / sum(r['t_dec'] for r in wide):.2f}x)")
    print(f"    {len(win)} wins worth {sum(r['t_dec'] - r['t'] for r in win) / 1e6:.0f} ms "
          f"(best {max(r['t_dec'] / r['t'] for r in win):.2f}x); "
          f"{len(lose)} losses worth {sum(r['t'] - r['t_dec'] for r in lose) / 1e6:.0f} ms "
          f"(worst {max(r['t'] / r['t_dec'] for r in lose):.0f}x slower)")
    print(f"    {sum(r['t'] - r['t_dec'] for r in lose) / sum(r['t_dec'] - r['t'] for r in win):.0f}x "
          f"more time wasted than saved")

    print("\n== 7. per-column safe thresholds: is one constant defensible? ==")
    for key, label in (("a", "k*covered_fraction"), ("cov", "covered_fraction")):
        line = []
        for col in cols.values():
            mine = [r for r in rows if r["short"] == col["short"] and r["arm"] == "spec"]
            best = 0.0
            for thresh in sorted({round(r[key], 6) for r in mine}):
                if all(r["win"] for r in mine if r[key] < thresh):
                    best = thresh
            line.append(f"{col['short']} {best:.4f}")
        print(f"  largest zero-false-positive threshold on {label:20s} " + "  ".join(line))

    print("\n== 8. what the shipped gate gets wrong ==")
    missed = [r for r in rows if not r["hint"] and r["win"]]
    speed = sorted(r["t_dec"] / r["t"] for r in missed)
    print(f"  false negatives {len(missed)}: declined a median {st.median(speed):.2f}x, "
          f"p90 {np.percentile(speed, 90):.2f}x, max {speed[-1]:.2f}x")
    why = collections.Counter()
    for r in missed:
        clauses = [c for c, hit in (("len<4", r["ln"] < 4), ("cost>16", r["cost"] > 16),
                                    ("cov>=1%", r["cov"] >= 0.01)) if hit]
        why[" + ".join(clauses) or "none"] += 1
    for key, count in why.most_common():
        print(f"    rejected by {key:28s} {count:5d}")

    print("\n== 9. what the row-table escape would change (excluded from the rule) ==")
    both = [r for r in rows if r["t_table"]]
    print(f"  ratio SIMD/table by cost bucket, {len(both)} queries:")
    for lo, hi in [(1, 16), (17, 32), (33, 64), (65, 128), (129, 512), (513, 1 << 30)]:
        mine = [r for r in both if lo <= r["cost"] <= hi]
        if not mine:
            continue
        v = sorted(r["t"] / r["t_table"] for r in mine)
        label = f"{lo}-{hi}" if hi < 1 << 30 else f"{lo}+"
        print(f"    cost {label:>10s} n={len(mine):4d}  median {st.median(v):6.2f}x  "
              f"p90 {np.percentile(v, 90):6.2f}x  max {v[-1]:7.1f}x")
    tabled = [r for r in both if r["cost"] > 16 and r["t"] / r["t_table"] > 1.15]
    beats = [r for r in tabled if r["t_table"] < r["T_base"]]
    print(f"  of the {len(tabled)} cost>16 covers the table speeds up by >1.15x, "
          f"{len(beats)} then beat the column baseline")
    for r in sorted(beats, key=lambda r: r["cost"])[:12]:
        print(f"    {r['short']:11s} cost {r['cost']:6.0f} cov {r['cov']:.5f}  "
              f"simd {r['t'] / 1e6:8.2f}ms -> table {r['t_table'] / 1e6:7.2f}ms  "
              f"baseline {r['T_base'] / 1e6:7.2f}ms")


if __name__ == "__main__":
    main()
