#!/usr/bin/env python3
"""Summarize fixed-cover matcher measurements; no coefficients are fitted."""
from __future__ import annotations

import argparse
from collections import Counter, defaultdict
import csv
import json
import math
from pathlib import Path
import statistics


def percentile(values, q):
    values = sorted(values)
    at = (len(values) - 1) * q
    lo = int(at)
    hi = min(lo + 1, len(values) - 1)
    return values[lo] + (values[hi] - values[lo]) * (at - lo)


def stats(values):
    return {"mean": statistics.mean(values), "p50": percentile(values, .5),
            "p90": percentile(values, .9), "p95": percentile(values, .95),
            "p99": percentile(values, .99), "max": max(values)}


def simple(query, point_limit):
    """Exploratory shape-only rule; constants are hypotheses, not calibrated facts."""
    eligible = {m["name"] for m in query["measurements"]}
    p, r = query["points"], query["ranges"]
    if p == 0:
        return "range" if "range" in eligible and r <= 8 else "table"
    if "eq_or" in eligible and p <= point_limit and p + 2 * r <= 32:
        return "eq_or"
    if "nibble_n8" in eligible and 4 * ((p + 7) // 8) + 2 * r <= 32:
        return "nibble_n8"
    return "table"


def summarize(queries, policy):
    times, ratios, oracle_ratios = [], [], []
    for q in queries:
        timings = {m["name"]: m["median_ns"] for m in q["measurements"]}
        forced = {k: v for k, v in timings.items() if k != "current"}
        selected = min(forced, key=forced.get) if policy == "oracle" else (
            simple(q, int(policy.split("_")[-1])) if policy.startswith("simple_") else policy)
        time = timings[selected]
        times.append(time / 1e6)
        ratios.append(time / timings["current"])
        oracle_ratios.append(time / min(forced.values()))
    return {"latency_ms": stats(times), "ratio_to_current": stats(ratios),
            "ratio_to_oracle": stats(oracle_ratios),
            "geomean_ratio_to_current": math.exp(statistics.mean(math.log(x) for x in ratios)),
            "sum_ratio_to_current": sum(times) / sum(next(m["median_ns"] for m in q["measurements"] if m["name"] == "current") / 1e6 for q in queries)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    args = parser.parse_args()
    manifest = json.loads((args.run / "manifest.json").read_text())
    if manifest["status"] != "complete":
        parser.error("run is incomplete; refusing to summarize partial results")
    summaries = {}
    cells = []
    lines = ["# Matcher selection measurements", "",
             "Prepared full-column scan times include matcher setup, row resolution and exact verification.",
             "The cover, encoded column and vector packing policy are held fixed for each query.",
             "Every measured variant passed the independent bitmap truth check.", "",
             "The measured oracle picks the fastest eligible forced matcher for each query. It is an",
             "optimistic, noise-sensitive lower bound, not an implementable selection rule. The simple",
             "shape rules below are exploratory comparisons on these same queries, not held-out validation.", ""]
    all_queries = []
    for case in manifest["cases"]:
        records = [json.loads(line) for line in (args.run / (case["id"] + ".jsonl")).read_text().splitlines()]
        queries = [r for r in records if r["type"] == "query"]
        setup = records[0]
        if records[-1]["type"] != "complete" or len(queries) != setup["queries"] or not all(q["correct"] for q in queries):
            parser.error(f"{case['id']}: missing or invalid results")
        all_queries.extend(queries)
        summaries[case["id"]] = {"setup": setup, "policies": {p: summarize(queries, p) for p in
            ("current", "table", "simple_2", "simple_4", "simple_8", "oracle")}}
        choices = Counter(next(m["matcher"] for m in q["measurements"] if m["name"] == "current") for q in queries)
        winners = Counter(min((m for m in q["measurements"] if m["name"] != "current"), key=lambda m: m["median_ns"])["name"] for q in queries)
        summaries[case["id"]]["selected_matchers"] = dict(choices)
        summaries[case["id"]]["measured_winners"] = dict(winners)
        # Sanity comparison: production selection versus forcing its identical configuration.
        same_ratios = []
        selection_regret = []
        worst = []
        for q in queries:
            current = next(m for m in q["measurements"] if m["name"] == "current")
            forced = next(m for m in q["measurements"] if m["name"] != "current" and m["selected"])
            same_ratios.append(forced["median_ns"] / current["median_ns"])
            best = min((m for m in q["measurements"] if m["name"] != "current"), key=lambda m: m["median_ns"])
            selection_regret.append(forced["median_ns"] / best["median_ns"])
            worst.append({"id":q["id"], "needle":q["needle"], "points":q["points"], "ranges":q["ranges"],
                          "row_selectivity":q["row_selectivity"], "probe_density":q["probe_density"],
                          "selected":current["matcher"], "fastest":best["name"],
                          "current_ms":current["median_ns"]/1e6, "fastest_ms":best["median_ns"]/1e6,
                          "ratio":current["median_ns"]/best["median_ns"]})
            for m in q["measurements"]:
                cells.append({"dataset":case["id"], "query":q["id"], "needle_len":q["needle_len"],
                    "matching_rows":q["matching_rows"], "row_selectivity":q["row_selectivity"],
                    "points":q["points"], "ranges":q["ranges"], "probe_density":q["probe_density"],
                    "variant":m["name"], "selected":m["selected"],
                    "skip_empty_packing":m["skip_empty_packing"], "median_ms":m["median_ns"]/1e6})
        summaries[case["id"]]["same_configuration_ratio"] = stats(same_ratios)
        summaries[case["id"]]["selected_matcher_regret"] = stats(selection_regret)
        summaries[case["id"]]["selected_within_5pct_of_fastest_fraction"] = sum(x <= 1.05 for x in selection_regret) / len(selection_regret)
        summaries[case["id"]]["largest_opportunities"] = sorted(worst, key=lambda q:q["ratio"], reverse=True)[:20]
        lines += [f"## {case['id']}", "", f"{len(queries)} queries, ISA {setup['isa']}.",
                  f"Current matcher choices: {dict(choices)}.", f"Measured winners: {dict(winners)}.", "",
                  "| Policy | Mean ms | P50 ms | P95 ms | P99 ms | Max ms | Sum/current | Worst/current |",
                  "|---|---:|---:|---:|---:|---:|---:|---:|"]
        for name, result in summaries[case["id"]]["policies"].items():
            s = result["latency_ms"]
            lines.append(f"| {name} | {s['mean']:.3f} | {s['p50']:.3f} | {s['p95']:.3f} | {s['p99']:.3f} | {s['max']:.3f} | {result['sum_ratio_to_current']:.3f} | {result['ratio_to_current']['max']:.3f} |")
        # Keep length/selectivity grouping so easy workloads cannot hide sparse cells.
        by_cell = defaultdict(list)
        for q in queries:
            cell = (q.get("meta") or {}).get("sa_lcp", {}).get("cell", -1)
            by_cell[cell].append(q)
        summaries[case["id"]]["cells"] = {str(c): {"queries":len(qs), "current":summarize(qs,"current"),
            "oracle":summarize(qs,"oracle")} for c,qs in sorted(by_cell.items())}
        lines += ["", "Forcing the currently selected configuration / production latency: "
                  f"median {percentile(same_ratios,.5):.3f}, P95 {percentile(same_ratios,.95):.3f}.", ""]
        lines += [f"Current matcher is within 5% of the fastest forced matcher for {100 * sum(x <= 1.05 for x in selection_regret) / len(selection_regret):.1f}% of queries.", ""]
    summaries["all_queries"] = {p:summarize(all_queries,p) for p in ("current","table","simple_2","simple_4","simple_8","oracle")}
    lines += ["## Exploratory simple rules", "",
              "For points-only or mixed covers, try EqOr with at most 2, 4 or 8 points and at most",
              "32 point/range comparison units (p + 2r); otherwise try eligible Nibble with",
              "4 ceil(p/8) + 2r <= 32, otherwise Table. For range-only covers use Range up to",
              "8 ranges, otherwise Table. These cutoffs are hypotheses for comparison, not recommendations.", "",
              "All queries are weighted equally in means and percentiles. The balanced grid is not a",
              "production workload distribution. Query preparation and compression times are excluded.", ""]
    (args.run / "summary.json").write_text(json.dumps(summaries, indent=2) + "\n")
    (args.run / "SUMMARY.md").write_text("\n".join(lines))
    with (args.run / "measurements.csv").open("w",newline="") as file:
        writer = csv.DictWriter(file, fieldnames=list(cells[0]))
        writer.writeheader()
        writer.writerows(cells)
    print(f"Summary: {args.run / 'SUMMARY.md'}")


if __name__ == "__main__":
    main()
