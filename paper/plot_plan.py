# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "duckdb>=1.1", "numpy"]
# ///
"""Size of the planner's minimum cut, from a plan log, as two panels one
column wide, stacked in the paper:

    plan-cut/selectivity.pdf  tokens in the cut against row selectivity, OnPair12,
                              OnPair16 and FSST, every needle length
    plan-cut/matrix.pdf       OnPair12's tokens in the cut per (selectivity,
                              needle length) cell, as the micro matrices

    uv run paper/plot_plan.py [--log results/plan-log/<tag>-<date>] [--out DIR]

Reads a log paper/gen-plan-log.sh writes, by default the one paper/inputs.toml
names. A cut is
sized by the tokens it matches: the union of the code ids of the steps it
takes, each id of a code range counted, plus one for each FSST code pair. Every
cut matches at least one token. One value per (row group, needle), the median
per bin or cell drawn, and in the line chart the 25th to 75th percentile as a
band behind it. Selectivity is the needle's over the whole column, the mean of
its two equal row groups, which is what the suite's grid was filled on. In the
line chart a bin some column has no needle in is drawn hollow.
"""

import argparse
import base64
import json
from pathlib import Path

import numpy as np
import pandas as pd

import paperfig as pf
from plot_micro import (MATRIX_CMAP, draw_matrix, matrix_colorbar, matrix_pivot,
                        selectivity_x)

ROOT = Path(__file__).resolve().parent.parent
# Colours and markers of the same systems in the micro figures.
ALGORITHMS = [("onpair16", "OnPair16", "#0072B2", "o"),
              ("onpair12", "OnPair12", "#56B4E9", "s"),
              ("fsst", "FSST", "#CC79A7", "P")]
LINE_H = 1.4
MATRIX_H = 1.95
# FSST's escape code: a cut escape step probes for it.
ESCAPE = 255


def cut_shape(edges):
    """Codes, code ranges and code pairs of one query's cut, as the scan issues
    them, and the tokens they match."""
    ids, pairs = set(), set()
    for kind, label, cut in edges[["label_type", "label", "cut"]].itertuples(index=False):
        if cut.startswith("paired:"):
            second = int(label) if kind == "code" else json.loads(label)[0]
            pairs.update((int(first), second) for first in cut[len("paired:"):].split(";"))
            continue
        if cut != "yes":
            continue
        if kind == "code":
            ids.add(int(label))
        elif kind == "escape-code":
            ids.add(ESCAPE)
        elif kind == "code-set":
            ids.update(json.loads(label))
        elif kind == "code-range":
            lo, hi = json.loads(label)
            ids.update(range(lo, hi + 1))
    ids = np.array(sorted(ids))
    runs = np.split(ids, np.where(np.diff(ids) != 1)[0] + 1) if len(ids) else []
    return (sum(len(r) == 1 for r in runs), sum(len(r) > 1 for r in runs), len(pairs),
            len(ids) + len(pairs))


def load(log):
    queries = pd.read_csv(log / "queries.csv")
    graph = pd.read_csv(log / "graph.csv", keep_default_na=False, dtype={"label": str})
    shapes = {uuid: cut_shape(g) for uuid, g in graph.groupby("uuid")}
    queries[["codes", "ranges", "pairs", "tokens"]] = [shapes[u] for u in queries["uuid"]]
    if (queries["tokens"] < 1).any():
        raise SystemExit("a cut matches no token")
    whole = queries.groupby(["algorithm", "dataset", "needle"])["needle_selectivity"].transform("mean")
    return queries.assign(
        needle_len=queries["needle"].map(lambda b: len(base64.b64decode(b))),
        selectivity=whole)


def selectivity_panel(d, out):
    """Median tokens in the cut per half-decade of selectivity, one line per
    algorithm, its middle half as a band."""
    d = selectivity_x(d)
    fig, ax = pf.panel(pf.COLUMN_IN, LINE_H)
    for algorithm, name, color, marker in ALGORITHMS:
        g = d[d["algorithm"] == algorithm].groupby("sel")
        lo, hi = g["tokens"].quantile(0.25), g["tokens"].quantile(0.75)
        ax.fill_between(lo.index, lo.values, hi.values, color=color, alpha=0.15, linewidth=0)
        med = g["tokens"].median()
        ax.plot(med.index, med.values, marker=marker, color=color, label=name)
    pf.selectivity_axis(ax, "row selectivity of needle")
    ax.set_yscale("log")
    ax.yaxis.set_major_formatter(pf.mpl.ticker.FuncFormatter(lambda v, _: f"{v:g}"))
    ax.yaxis.set_minor_formatter(pf.mpl.ticker.NullFormatter())
    ax.set_ylabel("tokens in the cut")
    ax.legend(loc="upper left")
    pf.save_panel(fig, "plan-cut", "selectivity", out)


def matrix_panel(d, out):
    """Median codes in OnPair12's cut per (selectivity, needle length) cell,
    drawn as the micro matrices are, its colour scale beside it."""
    grid = matrix_pivot(d[d["algorithm"] == "onpair12"], "tokens", "median")
    drawn = grid.values[~np.isnan(grid.values)]
    norm = pf.mpl.colors.LogNorm(vmin=max(1.0, drawn.min()), vmax=drawn.max())
    fig, ax = pf.panel(pf.COLUMN_IN, MATRIX_H)
    draw_matrix(fig, ax, grid, norm)
    bar = fig.colorbar(pf.mpl.cm.ScalarMappable(norm=norm, cmap=MATRIX_CMAP), ax=ax,
                       pad=0.02, label="number of codes")
    matrix_colorbar(bar, norm, bar.ax.yaxis)
    pf.save_panel(fig, "plan-cut", "matrix", out)
    return grid


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--log", type=Path, default=None)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    log = args.log or ROOT / pf.inputs("plan")["log"]
    d = load(log)
    print(f"{log}: {len(d)} (row group, needle, algorithm) cells")
    by_sel = selectivity_x(d)
    for q in (0.25, 0.5, 0.75):
        print(f"\ntokens in the cut per selectivity bin, quantile {q}")
        print(by_sel.pivot_table(index="sel", columns="algorithm", values="tokens",
                                 aggfunc=lambda v: v.quantile(q)).to_string())

    pf.clear_figure("plan-cut", args.out)
    selectivity_panel(d, args.out)
    grid = matrix_panel(d, args.out)
    print("\nmedian tokens per cell, onpair12 (rows: selectivity buckets, columns: length buckets)")
    print(grid.to_string())


if __name__ == "__main__":
    main()
