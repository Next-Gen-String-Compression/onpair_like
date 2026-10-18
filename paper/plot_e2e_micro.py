# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "numpy", "duckdb"]
# ///
"""Scan throughput against needle selectivity for the micro benchmark's
needles run end to end, from an `e2e/micro.py` run: one
`SELECT count(*) ... LIKE '%needle%'` per needle on the four systems of the
end-to-end benchmark, each needle's fastest run by its profile's latency,
throughput the column's uncompressed bytes over that latency.

    uv run paper/plot_e2e_micro.py [--run RUN] [--uncompressed RUN] [--out DIR]

The runs default to config.INPUTS["e2e-micro"] and are read from
results/e2e/<run>/. Of the uncompressed run, which holds DuckDB and Vortex on
uncompressed copies of the columns, DuckDB is drawn: Vortex validates every
string as UTF-8 on each decode of an uncompressed column, which is most of
its scan. Writes `e2e-micro-datasets/`, one
panel per column with every needle as a point and the median per selectivity
bracket as the line, ClickBench q20's needle (`google`, query `q20`) marked
on the URL panel and left out of the medians, plus `legend.pdf`, and
`e2e-micro-pooled/all.pdf` and `legend.pdf`, every column pooled without q20,
the median per bracket with its middle half as the band.
"""

import argparse
from pathlib import Path

import pandas as pd

import paperfig as pf
from config import COLOR, ENGINE, METHOD, SCHEME
from plot_e2e import SYSTEMS as E2E_SYSTEMS
from plot_micro import DATASET_ORDER, TITLES, dataset_width, gbs_axis, selectivity_x

ROOT = Path(__file__).resolve().parent.parent
# DuckDB on uncompressed copies of the columns, before the four of e2e/run.py.
SYSTEMS = [("duckdb-uncompressed", f"{ENGINE['duckdb']}, {SCHEME['uncompressed']}", COLOR["uncompressed"]),
           *E2E_SYSTEMS]
LABEL = {system: label for system, label, _ in SYSTEMS}
# One block per engine and scheme, its name before its methods, all in one row.
LEGEND_GROUPS = [
    (f"{ENGINE['duckdb']}, {SCHEME['fsst']}", [(LABEL["duckdb-uncompressed"], SCHEME["uncompressed"].lower()),
                                               (LABEL["duckdb"], METHOD["decode"]),
                                               (LABEL["duckdb-contains"], METHOD["cut"])]),
    (f"{ENGINE['vortex']}, {SCHEME['onpair']}", [(LABEL["vortex"], METHOD["decode"]),
                                                 (LABEL["vortex-onpair"], METHOD["cut"])]),
]
MARKERS = {"duckdb-uncompressed": "D",
           "duckdb": "v", "duckdb-contains": "^", "vortex": "s", "vortex-onpair": "o"}
# ClickBench q20's needle, which e2e/micro.py adds to the URL column
Q20 = "q20"


def load(*runs):
    d = pd.concat([pd.read_csv(ROOT / "results" / "e2e" / run / "needles.csv") for run in runs], ignore_index=True)
    d = d[d["system"].isin(LABEL)]
    best = pf.fastest(d, ["system", "dataset", "query"], "latency_s")
    return selectivity_x(best.assign(gbps=best["payload_bytes"] / best["latency_s"] / 1e9))


def datasets_panel(ax, d):
    q20 = d[d["query"] == Q20]
    d = d[d["query"] != Q20]
    for system, label, color in SYSTEMS:
        g = d[d["system"] == system]
        ax.scatter(g["selectivity"], g["gbps"], s=4, alpha=0.25, linewidths=0, color=color,
                   marker=MARKERS[system])
        med = g.groupby("sel")["gbps"].median()
        ax.plot(med.index, med.values, marker=MARKERS[system], color=color, label=label)
        q = q20[q20["system"] == system]
        ax.scatter(q["selectivity"], q["gbps"], s=4, color=color, marker=MARKERS[system],
                   edgecolors="black", linewidths=0.4, zorder=5)
    if len(q20):
        top = q20.loc[q20["gbps"].idxmax()]
        ax.annotate("Q20", (top["selectivity"], top["gbps"]), xytext=(4, 0), textcoords="offset points",
                    ha="left", va="center_baseline", fontsize=pf.SMALL_PT - 1.5, zorder=6,
                    bbox={"boxstyle": "square,pad=0.15", "facecolor": "white", "edgecolor": "none"})
    pf.selectivity_axis(ax)
    ax.set_ylabel("uncompressed GB/s")


def pooled_panel(ax, d):
    g = d.groupby(["system", "sel"])["gbps"]
    med, lo, hi = g.median(), g.quantile(0.25), g.quantile(0.75)
    for system, label, color in SYSTEMS:
        m = med.loc[system]
        ax.fill_between(m.index, lo.loc[system].values, hi.loc[system].values, color=color, alpha=0.15,
                        linewidth=0)
        ax.plot(m.index, m.values, marker=MARKERS[system], color=color, label=label)
    pf.selectivity_axis(ax)
    ax.set_ylabel("uncompressed GB/s")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", default=None)
    ap.add_argument("--uncompressed", default=None)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    runs = pf.inputs("e2e-micro")
    d = load(args.run or runs["run"], args.uncompressed or runs["uncompressed"])
    print(d.pivot_table(index="dataset", columns="system", values="gbps", aggfunc="median").round(2).to_string())

    drawn = [k for k in DATASET_ORDER if k in set(d["dataset"])]
    width = dataset_width(len(drawn))
    pf.clear_figure("e2e-micro-datasets", args.out)
    panels = []
    for dataset in drawn:
        fig, ax = pf.panel(width=width)
        datasets_panel(ax, d[d["dataset"] == dataset])
        panels.append((fig, ax))
    gbs_axis([ax for _, ax in panels], figure="e2e-micro-datasets")
    pf.save_row(panels, "e2e-micro-datasets", [pf.slug(TITLES[k]) for k in drawn], width, args.out)
    pf.save_grouped_legend(panels[-1][1], LEGEND_GROUPS, "e2e-micro-datasets", args.out, inline=True)

    pf.clear_figure("e2e-micro-pooled", args.out)
    fig, ax = pf.panel(width=pf.COLUMN_IN)
    pooled_panel(ax, d[d["query"] != Q20])
    gbs_axis(ax)
    pf.save_row([(fig, ax)], "e2e-micro-pooled", ["all"], pf.COLUMN_IN, args.out, unit=pf.COLUMN_IN)
    pf.save_grouped_legend(ax, LEGEND_GROUPS, "e2e-micro-pooled", args.out, width=pf.COLUMN_IN)


if __name__ == "__main__":
    main()
