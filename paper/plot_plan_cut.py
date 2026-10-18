# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "duckdb>=1.1", "numpy"]
# ///
"""Size of the planner's cut, from a plan log, as one matrix per
OnPair variant, side by side as the micro matrices are:

    plan-cut/onpair12.pdf  tokens in OnPair12's cut per (selectivity, needle
                           length) cell
    plan-cut/onpair16.pdf  the same for OnPair16
    plan-cut/legend.pdf    their shared colour scale
    plan-cut/sizes.pdf     mean codes (solid) and code ranges (dashed) in the
                           cut per selectivity bracket, one colour per encoding

    uv run paper/plot_plan_cut.py [--log results/plan-log/<tag>-<date>] [--out DIR]

Reads a log paper/gen-plan-cut.sh writes, by default the one paper/config.py
names. A cut is
sized by the tokens it matches: the union of the code ids of the steps it
takes, each id of a code range counted, plus one for each FSST code pair. Every
cut matches at least one token. One value per (row group, needle), the median
per cell drawn. Selectivity is the needle's over the whole column, the mean of
its two equal row groups, which is what the suite's grid was filled on.
"""

import argparse
import base64
import json
from pathlib import Path

import numpy as np
import pandas as pd

import paperfig as pf
from config import COLOR, SCHEME
from plot_micro import (MATRIX_CMAP, draw_matrix, matrix_legend, matrix_pivot,
                        selectivity_x)

ROOT = Path(__file__).resolve().parent.parent
MATRIX_ALGORITHMS = ["onpair12", "onpair16"]
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


def matrix_panels(d, out):
    """Median tokens in the cut per (selectivity, needle length) cell, one
    matrix per OnPair variant on one colour scale, drawn as the micro
    matrices are."""
    grids = [(a, matrix_pivot(d[d["algorithm"] == a], "tokens", "median"))
             for a in MATRIX_ALGORITHMS]
    drawn = np.concatenate([g.values[~np.isnan(g.values)] for _, g in grids])
    norm = pf.mpl.colors.LogNorm(vmin=max(1.0, drawn.min()), vmax=drawn.max())
    panels = []
    for _, grid in grids:
        fig, ax = pf.panel(height=1.95)
        draw_matrix(fig, ax, grid, norm)
        if panels:
            ax.set_yticklabels([])
        panels.append((fig, ax))
    pf.save_row(panels, "plan-cut", MATRIX_ALGORITHMS, pf.PANEL_4UP, out, gap=0)
    matrix_legend(norm, MATRIX_CMAP, out, "plan-cut", "number of codes",
                  label_above=True)
    return grids


# Colours as the systems in plot_micro.SYSTEMS.
SIZE_ALGORITHMS = [("fsst", SCHEME["fsst"], COLOR["fsst-cut"]), ("onpair12", SCHEME["onpair12"], COLOR["onpair12-cut"]),
                   ("onpair16", SCHEME["onpair16"], COLOR["onpair16-cut"])]


def sizes_panel(d, out):
    """Mean codes and code ranges in the cut per selectivity bracket. An FSST
    code pair counts as one code."""
    d = selectivity_x(d.assign(single=d["codes"] + d["pairs"]))
    fig, ax = pf.panel(width=pf.COLUMN_IN, height=1.65)
    for algorithm, name, color in SIZE_ALGORITHMS:
        m = d[d["algorithm"] == algorithm].groupby("sel")[["single", "ranges"]].mean()
        print(f"\nmean cut size per bracket, {algorithm}\n{m.round(2).to_string()}")
        ax.plot(m.index, m["single"], color=color, marker="o", label=name)
        r = m["ranges"][m["ranges"] > 0]
        ax.plot(r.index, r.values, color=color, marker="o", linestyle="--", markerfacecolor="white")
    ax.plot([], [], color="black", label="codes")
    ax.plot([], [], color="black", linestyle="--", label="ranges")
    pf.selectivity_axis(ax)
    pf.y_axis(ax)
    ax.set_ylabel("mean per cut")
    # matplotlib fills a legend column first: interleave so each row reads across.
    h, l = ax.get_legend_handles_labels()
    order = [0, 3, 1, 4, 2]
    ax.legend([h[i] for i in order], [l[i] for i in order], loc="lower left", bbox_to_anchor=(0, 1.02), ncol=3, handlelength=1.6,
              columnspacing=1.0, borderaxespad=0)
    pf.save(fig, "plan-cut/sizes", out)
    pf.plt.close(fig)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--log", type=Path, default=None)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    log = args.log or ROOT / pf.inputs("plan-cut")["log"]
    d = load(log)
    print(f"{log}: {len(d)} (row group, needle, algorithm) cells")
    by_sel = selectivity_x(d)
    for q in (0.25, 0.5, 0.75):
        print(f"\ntokens in the cut per selectivity bin, quantile {q}")
        print(by_sel.pivot_table(index="sel", columns="algorithm", values="tokens",
                                 aggfunc=lambda v: v.quantile(q)).to_string())

    pf.clear_figure("plan-cut", args.out)
    sizes_panel(d, args.out)
    for algorithm, grid in matrix_panels(d, args.out):
        print(f"\nmedian tokens per cell, {algorithm} (rows: selectivity buckets, columns: length buckets)")
        print(grid.to_string())


if __name__ == "__main__":
    main()
