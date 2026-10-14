# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "numpy", "duckdb"]
# ///
"""Scan throughput against needle selectivity for the micro benchmark's
needles run end to end, from an `e2e/micro.py` run: one
`SELECT count(*) ... LIKE '%needle%'` per needle on the four systems of the
end-to-end benchmark, each needle's fastest run by its profile's latency,
throughput the column's uncompressed bytes over that latency.

    uv run paper/plot_e2e_micro.py [--run RUN] [--out DIR]

The run defaults to config.INPUTS["e2e-micro"] and is read from
results/e2e/<run>/. Writes `e2e-micro-datasets/`, one panel per column with
every needle as a point and the median per selectivity bracket as the line,
plus `legend.pdf`, and `e2e-micro-pooled/all.pdf` and `legend.pdf`, every
column pooled, the median per bracket with its middle half as the band.
"""

import argparse
from pathlib import Path

import pandas as pd

import paperfig as pf
from plot_e2e import SYSTEMS
from plot_micro import DATASET_ORDER, TITLES, dataset_width, gbs_axis, selectivity_x

ROOT = Path(__file__).resolve().parent.parent
MARKERS = {"duckdb": "v", "duckdb-contains": "^", "vortex": "s", "vortex-onpair": "o"}


def load(run):
    d = pd.read_csv(ROOT / "results" / "e2e" / run / "needles.csv")
    best = d.loc[d.groupby(["system", "dataset", "query"])["latency_s"].idxmin()]
    return selectivity_x(best.assign(gbps=best["payload_bytes"] / best["latency_s"] / 1e9))


def datasets_panel(ax, d):
    for system, label, color in SYSTEMS:
        g = d[d["system"] == system]
        ax.scatter(g["selectivity"], g["gbps"], s=4, alpha=0.25, linewidths=0, color=color,
                   marker=MARKERS[system])
        med = g.groupby("sel")["gbps"].median()
        ax.plot(med.index, med.values, marker=MARKERS[system], color=color, label=label)
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
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    d = load(args.run or pf.inputs("e2e-micro")["run"])
    print(d.pivot_table(index="dataset", columns="system", values="gbps", aggfunc="median").round(2).to_string())

    drawn = [k for k in DATASET_ORDER if k in set(d["dataset"])]
    width = dataset_width(len(drawn))
    pf.clear_figure("e2e-micro-datasets", args.out)
    panels = []
    for dataset in drawn:
        fig, ax = pf.panel(width=width)
        datasets_panel(ax, d[d["dataset"] == dataset])
        panels.append((fig, ax))
    gbs_axis([ax for _, ax in panels])
    pf.save_row(panels, "e2e-micro-datasets", [pf.slug(TITLES[k]) for k in drawn], width, args.out)
    pf.save_legend(panels[-1][1], "e2e-micro-datasets", 4, args.out)

    pf.clear_figure("e2e-micro-pooled", args.out)
    fig, ax = pf.panel(width=pf.COLUMN_IN)
    pooled_panel(ax, d)
    gbs_axis(ax)
    pf.save_row([(fig, ax)], "e2e-micro-pooled", ["all"], pf.COLUMN_IN, args.out, unit=pf.COLUMN_IN)
    pf.save_legend(ax, "e2e-micro-pooled", 2, args.out, width=pf.COLUMN_IN)


if __name__ == "__main__":
    main()
