# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "numpy"]
# ///
"""Runtime and speedup distributions of the end-to-end benchmark over the
queries (or LIKE scans) of a workload, from an `e2e/run.py` run, each query's
fastest run by its profile's latency. The runtime figures draw one box per
system; the speedup figures one per system pair, the baseline's time over the
cut scan's: DuckDB with FSST over DuckDB with the FSST cut scan, and Vortex
0.86.1 over Vortex with the OnPair cut scan, for the items whose baseline takes
at least 1 ms. ClickBench q00 and q06 are answered from metadata
and write no profile, so theirs is the shell's wall time.

    uv run paper/plot_e2e.py [--run RUN] [--out DIR]

The run defaults to config.INPUTS["e2e"] and is read from
results/e2e/<run>/. Writes `e2e-queries/` and `e2e-queries-speedup/`, from
queries.csv, the queries whose SQL has a LIKE, and `e2e-scans/` and `e2e-scans-speedup/`, from scans.csv: every table scan whose filters, as the profile
prints them, hold a LIKE (DuckDB rewrites one into contains, prefix, suffix or
~~), its operator time in the query's fastest run, CPU time over all threads.
Each `job.pdf`, `clickbench.pdf` when the run has ClickBench (e2e/run.py
--workloads clickbench,job), and the runtime figures `legend.pdf`.
`e2e-job-duckdb/` is the DuckDB pair on JOB alone, `queries.pdf` and
`scans.pdf`, each a horizontal box on its own linear axis. Every figure is drawn
twice: DuckDB's pair as run, and again from the pair with compressed equality on
in both arms (e2e/run.py's duckdb-eq and duckdb-contains-eq), named with -eq,
when the run has it. A LIKE a system evaluates above the scan rather
than in it is not counted, so the two systems' scans can differ; a system and
its baseline share their plans.
"""

import argparse
import json
import re
from pathlib import Path

import numpy as np
import pandas as pd

import paperfig as pf
from config import COLOR, ENGINE, METHOD, SCHEME

ROOT = Path(__file__).resolve().parent.parent
# Named and coloured like the micro series they stand for, config.COLOR.
SYSTEMS = [
    ("duckdb", f"{ENGINE['duckdb']}, {SCHEME['fsst']} {METHOD['decode']}", COLOR["fsst-decode"]),
    ("duckdb-contains", f"{ENGINE['duckdb']}, {SCHEME['fsst']} {METHOD['cut']}", COLOR["fsst-cut"]),
    ("vortex", f"{ENGINE['vortex']}, {METHOD['decode']}", COLOR["onpair-decode"]),
    ("vortex-onpair", f"{ENGINE['vortex']}, {SCHEME['onpair']} {METHOD['cut']}", COLOR["onpair16-cut"]),
]
# baseline, cut scan, label, colour
PAIRS = [("duckdb", "duckdb-contains", ENGINE["duckdb"], COLOR["fsst-cut"]),
         ("vortex", "vortex-onpair", ENGINE["vortex"], COLOR["onpair16-cut"])]
WORKLOADS = [("clickbench", "ClickBench"), ("job", "JOB")]
# figure name suffix, the arms that stand in for duckdb and duckdb-contains
BASELINES = [("", {}), ("-eq", {"duckdb-eq": "duckdb", "duckdb-contains-eq": "duckdb-contains"})]
# The speedup figures drop items whose baseline takes less than this.
MIN_S = 1e-3
SPEEDUP_TICKS = [m * 10.0 ** e for e in range(-2, 4) for m in (1, 2, 5)]
# figure, data, y label, speedup
FIGURES = [("e2e-queries", "e2e-queries", "query time [ms]", False),
           ("e2e-scans", "e2e-scans", "scan time [ms]", False),
           ("e2e-queries-speedup", "e2e-queries", "query speedup", True),
           ("e2e-scans-speedup", "e2e-scans", "scan speedup", True)]
LIKE = {"duckdb": r"contains\(|prefix\(|suffix\(|~~", "vortex": r" like "}


def load(run):
    """{figure: (system, workload, item, seconds)}, an item a query or a scan,
    from the query's fastest run."""
    path = ROOT / "results" / "e2e" / run
    q = pd.read_csv(path / "queries.csv")
    s = pd.read_csv(path / "scans.csv", keep_default_na=False)
    q["seconds"] = q["latency_s"].fillna(q["time_s"])
    best = pf.fastest(q, ["system", "workload", "query"], "seconds")
    s = s.merge(best[["system", "workload", "query", "run"]])
    like = [bool(re.search(LIKE[system.split("-")[0]], f)) for system, f in zip(s["system"], s["filters"])]
    s = s[like].assign(item=lambda x: x["query"] + "/" + x["scan"].astype(str))
    # Queries whose SQL, as DuckDB's plan records it, has a LIKE.
    sql = {(w, f.stem): json.loads(f.read_text())["query"]["sql"]
           for w, _ in WORKLOADS for f in (path / "plans" / "duckdb" / w).glob("*.json")}
    has_like = [bool(re.search(r"\blike\b", sql.get(k, ""), re.I)) for k in zip(best["workload"], best["query"])]
    best = best[has_like]
    return {
        "e2e-queries": best.rename(columns={"query": "item"})[["system", "workload", "item", "seconds"]],
        "e2e-scans": s[["system", "workload", "item", "seconds"]],
    }


def summary(name, d):
    ms = d.pivot_table(index=["workload", "item"], columns="system", values="seconds") * 1000
    print(f"{name}: count, median ms, and the median and max speedup over the system's baseline")
    for workload, g in ms.groupby(level="workload"):
        print(f"  {workload:10} " + "  ".join(f"{s} {g[s].count()} {g[s].median():.1f}" for s, _, _ in SYSTEMS))
        for base, cand in (("duckdb", "duckdb-contains"), ("vortex", "vortex-onpair")):
            r = (g[base] / g[cand]).dropna()
            print(f"  {'':10} {cand} {r.median():.2f}x, max {r.max():.2f}x ({r.idxmax()[1]})")


def boxes(ax, data, colors, labels, widths, fmt):
    """The mean as a white diamond, its value written beside the box."""
    b = ax.boxplot(data, widths=widths, patch_artist=True, showfliers=True, showmeans=True,
                   meanprops={"marker": "D", "markersize": 2.5, "markerfacecolor": "white",
                              "markeredgecolor": "black", "markeredgewidth": 0.5},
                   medianprops={"color": "black", "linewidth": 0.8},
                   whiskerprops={"linewidth": 0.6}, capprops={"linewidth": 0.6},
                   flierprops={"marker": "o", "markersize": 3, "markeredgecolor": "black",
                               "markeredgewidth": 0.5})
    for box, flier, color, label in zip(b["boxes"], b["fliers"], colors, labels):
        box.set(facecolor=color, edgecolor="black", linewidth=0.6, label=label)
        flier.set(markerfacecolor=color)
    for i, x in enumerate(data, 1):
        ax.annotate(fmt.format(np.mean(x)), (i + widths / 2, np.mean(x)), xytext=(1.5, 0),
                    textcoords="offset points", ha="left", va="center", fontsize=pf.SMALL_PT)
    # Room on the right for the last box's mean.
    ax.set_xlim(0.5, len(data) + 0.75)
    ax.grid(axis="x", visible=False)


def panel(d, workload, title, ylabel, speedup):
    fig, ax = pf.panel(width=0.5 * pf.COLUMN_IN, height=1.6)
    if speedup:
        g = d[d["workload"] == workload].pivot_table(index="item", columns="system", values="seconds")
        ax.axhline(1, color="#BBBBBB", linewidth=0.6, zorder=0)
        # Below a millisecond the ratio is mostly the clock and the scheduler.
        r = [(g[base] / g[cand])[g[base] >= MIN_S].dropna() for base, cand, _, _ in PAIRS]
        boxes(ax, r, [c for *_, c in PAIRS], [label for _, _, label, _ in PAIRS], 0.25,
              "{:.2f}$\\times$")
        ax.set_xticks([1, 2], [f"{label}\n(n = {len(x)})" for (_, _, label, _), x in zip(PAIRS, r)])
    else:
        g = d[d["workload"] == workload]
        boxes(ax, [g.loc[g["system"] == s, "seconds"] * 1000 for s, _, _ in SYSTEMS],
              [c for *_, c in SYSTEMS], [label for _, label, _ in SYSTEMS], 0.3,
              "{:.1f}")
        ax.set_xticks([])
    ax.set_xlabel(title)
    ax.set_ylabel(ylabel)
    return fig, ax


def job_duckdb(data, suffix, out):
    """`e2e-job-duckdb<suffix>/{queries,scans}`: DuckDB with FSST over DuckDB with the
    FSST cut scan on JOB, the queries and the LIKE scans each one horizontal
    box on its own linear axis, outliers as points."""
    base, cand, _, color = PAIRS[0]
    pf.clear_figure("e2e-job-duckdb" + suffix, out)
    panels = []
    for source, label in (("e2e-queries", "query"), ("e2e-scans", "scan")):
        d = data[source]
        g = d[d["workload"] == "job"].pivot_table(index="item", columns="system", values="seconds")
        r = (g[base] / g[cand])[g[base] >= MIN_S].dropna()
        print(f"e2e-job-duckdb{suffix} {label}: n {len(r)}, median {r.median():.2f}x, "
              f"geomean {np.exp(np.log(r).mean()):.2f}x, slower {int((r < 1).sum())}, "
              f"min {r.min():.2f}x, max {r.max():.2f}x")
        fig, ax = pf.panel(width=pf.COLUMN_IN, height=0.8)
        ax.axvline(1, color="#BBBBBB", linewidth=0.6, zorder=0)
        ax.boxplot([r], vert=False, widths=0.2, patch_artist=True, showfliers=True,
                   boxprops={"facecolor": color, "edgecolor": "black", "linewidth": 0.6},
                   medianprops={"color": "black", "linewidth": 0.8},
                   whiskerprops={"linewidth": 0.6}, capprops={"linewidth": 0.6},
                   flierprops={"marker": "o", "markersize": 3, "markerfacecolor": color,
                               "markeredgecolor": "black", "markeredgewidth": 0.5})
        # save_row measures the first y tick, so one stays, unlabelled.
        ax.set_yticks([1], [""])
        ax.tick_params(axis="y", length=0)
        ax.set_ylim(0.6, 1.6)
        # The mean as a line taller than the box, its value beside it.
        ax.plot([r.mean()] * 2, [0.7, 1.55], color="black", linewidth=0.8, zorder=4)
        ax.annotate(f"mean {r.mean():.2f}$\\times$", (r.mean(), 1.55), xytext=(2, 0),
                    textcoords="offset points", ha="left", va="top", fontsize=pf.SMALL_PT)
        pad = 0.04 * (r.max() - min(r.min(), 1))
        lo, hi = min(r.min(), 1) - pad, r.max() + pad
        ax.set_xlim(lo, hi)
        # 1x always a tick, a nice tick too close to it dropped.
        nice = pf.mpl.ticker.MaxNLocator(5, steps=[1, 2, 5]).tick_values(lo, hi)
        ax.set_xticks([1] + [t for t in nice if lo <= t <= hi and abs(t - 1) > 0.1 * (hi - lo)])
        ax.xaxis.set_major_formatter(pf.mpl.ticker.FuncFormatter(lambda v, _: f"{v:g}$\\times$"))
        ax.grid(axis="y", visible=False)
        # The subcaption carries what the x label would: "<label> speedup (n = ...)".
        print(f"e2e-job-duckdb{suffix} {label}: subcaption {label.capitalize()} speedup (n = {len(r)})")
        panels.append((fig, ax))
    # Stacked in the paper; save_row gives the two the same axes width.
    pf.save_row(panels, "e2e-job-duckdb" + suffix, ["queries", "scans"], pf.COLUMN_IN, out, unit=pf.COLUMN_IN,
                gap=0)


def draw(data, suffix, out):
    # A panel per workload the run holds: a default run has JOB only.
    workloads = [(w, t) for w, t in WORKLOADS if (data["e2e-queries"]["workload"] == w).any()]
    for name, source, ylabel, speedup in FIGURES:
        d = data[source]
        if not speedup:
            summary(source + suffix, d)
        pf.clear_figure(name + suffix, out)
        titles = [f"{t}, {d.loc[d['workload'] == w, 'item'].nunique()} queries" if source == "e2e-queries" and not speedup else t
                  for w, t in workloads]
        panels = [panel(d, w, t, ylabel, speedup) for (w, _), t in zip(workloads, titles)]
        # Each panel its own range, but for the scan speedups, which share one.
        axes = [ax for _, ax in panels]
        for ax in axes:
            if speedup:
                pf.y_axis(ax, ticks=SPEEDUP_TICKS, fmt="{:g}$\\times$", minor=False)
            else:
                pf.y_axis(ax)
        if name == "e2e-scans-speedup":
            lims = [ax.get_ylim() for ax in axes]
            for ax in axes:
                ax.set_ylim(min(lo for lo, _ in lims), max(hi for _, hi in lims))
        pf.save_row(panels, name + suffix, [w for w, _ in workloads], 0.5 * pf.COLUMN_IN, out, unit=pf.COLUMN_IN)
        if not speedup:
            pf.save_legend(panels[-1][1], name + suffix, 4, out)
    job_duckdb(data, suffix, out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", default=None)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    data = load(args.run or pf.inputs("e2e")["run"])
    stand_ins = {s for _, arms in BASELINES for s in arms}
    for suffix, arms in BASELINES:
        if not set(arms) <= set(data["e2e-queries"]["system"]):
            print(f"no {', '.join(arms)} in the run: skipping the {suffix} figures")
            continue
        # the stand-ins under the names of the arms they replace, every other stand-in dropped
        variant = {k: d[~d["system"].isin(stand_ins - set(arms)) & ~d["system"].isin(arms.values())]
                   .replace({"system": arms}) for k, d in data.items()}
        draw(variant, suffix, args.out)


if __name__ == "__main__":
    main()
