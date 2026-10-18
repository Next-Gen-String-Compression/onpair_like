# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "numpy>=1.26", "duckdb>=1.1"]
# ///
"""Where a prefiltered query's time goes as the needle gets less selective,
over every column pooled (`phase-shares`).

    uv run paper/plot_phase_shares.py [--out DIR]

    phase-shares/onpair12-arm-neon.pdf, ... one per entry of [phases]
    phase-shares/legend.pdf, phase-shares/legend-unsplit.pdf

One column-wide panel per entry under "phases" in paper/config.py: a
series by its harness label (OnPair12, OnPair16 or the FSST port), the run
it is read from and, where the split bench measured one, the CSV that
divides its scan. The paper shows OnPair16 on NEON and says the others look
the same; the rest are drawn for the assets only.

One panel: each phase's time per row of the column, in ns, against needle
selectivity on a log axis: the per-query prep, the minimum cut scan and the
alignment walk that verifies the hits; the query's total in black under
config.PHASE_TOTAL_LINE. All are
the candidate's instrumented phases, each the fastest of five warm passes, so
the whole is their sum and not the timed query: for OnPair12 1.02x below it
on the M4 Pro, 1.10x in the median on the EPYC, where a cold query over a
column larger than the cache pays more.

The scan is one fused timing (`prefilter_ns`, the walk off). A split CSV
only divides it into matcher and resolver: each needle's scan is shared
between the two in the proportion the split bench measured them in apart,
since two separate passes do not sum to the fused one (0.98x of it on the M4
Pro, 0.88x on the EPYC, OnPair12). A panel without a split draws the scan as
one band and has its own legend, legend-unsplit.pdf.

Shares are taken per needle and averaged over a selectivity bracket
(paperfig.selectivity_bracket), so every query counts once; shares of the bin's summed time
would be DBpedia's, whose scan is six times the next column's.
"""

import argparse
from pathlib import Path

import duckdb
import numpy as np
import pandas as pd
import matplotlib.pyplot as plt

import paperfig as pf

HERE = Path(__file__).resolve().parent
DB = HERE.parent / "results" / "bench.duckdb"
FIGURE = "phase-shares"
INPUTS = pf.inputs("phases")
# The row group paper/specs/phases/verify-*.toml measures at, and the one every
# micro figure but the sweep is read at.
ROW_GROUP = 524288

COLOR = {
    "prep": "#262626",
    "matcher": "#D55E00",
    "resolver": "#009E73",
    "scan": "#D55E00",
    "walk": "#0072B2",
}
LABEL = {"prep": "compute cut", "matcher": "scan cut", "resolver": "resolve row", "walk": "verify hit",
         "scan": "scan cut + resolve row"}
SPLIT_STAGES = ["prep", "matcher", "resolver", "walk"]
FUSED_STAGES = ["prep", "scan", "walk"]


def load(run, label):
    con = duckdb.connect(str(DB), read_only=True)
    df = con.sql(
        """
        select dataset, query_id, selectivity, num_rows,
               coalesce(setup_ns, 0) as setup_ns,
               coalesce(prefilter_ns, 0) as prefilter_ns,
               coalesce(verify_ns, 0) as verify_ns
        from v
        where run_name = ? and is_latest and label = ?
          and status = 'ok' and chunk_rows = ?
        """,
        params=[run, label, ROW_GROUP],
    ).df()
    if df.empty:
        raise SystemExit(f"{run} has no {label} at {ROW_GROUP} rows")
    return df


def per_million_rows(df, split_csv=None):
    """One row per needle, every phase in ms per million rows of its column."""
    walk = df.set_index("query_id")
    unit = walk["num_rows"] / 1e6
    out = pd.DataFrame(index=walk.index)
    out["selectivity"] = walk["selectivity"]
    out["prep"] = walk["setup_ns"] / 1e6 / unit
    out["walk"] = walk["verify_ns"] / 1e6 / unit
    scan = walk["prefilter_ns"] / 1e6 / unit

    if split_csv is None:
        out["scan"] = scan
    else:
        split = pd.read_csv(split_csv).set_index("query_id")
        missing = walk.index.difference(split.index)
        if len(missing):
            raise SystemExit(f"{len(missing)} needles are not in {split_csv}")
        split = split.reindex(walk.index)
        matcher_share = split["matcher_ns"] / (split["matcher_ns"] + split["resolver_ns"])
        out["matcher"] = scan * matcher_share
        out["resolver"] = scan * (1 - matcher_share)

    out["x"] = pf.selectivity_bracket(out["selectivity"])
    return out


def share_panel(d, stages):
    """d: one row per needle, each stage in ms per million rows, binned at x."""
    shares = d[stages].div(d[stages].sum(axis=1), axis=0)
    bins = shares.groupby(d["x"]).mean()
    ms = d.groupby("x")[stages].mean()
    ms["total"] = ms.sum(axis=1)
    print(pd.concat([bins, ms.add_prefix("ms_")], axis=1).round(3).to_string())
    pf.style()
    fig, ax = plt.subplots(figsize=(pf.COLUMN_IN, 1.15), constrained_layout=True)
    x = ms.index.to_numpy()
    linear = pf.config.LINEAR_Y and FIGURE not in pf.config.LOG_FIGURES
    if pf.config.PHASE_STACKED and linear:
        # Top to bottom planning, matcher, resolver, verify, the legend in the
        # same order; stackplot stacks from the bottom.
        order = stages[::-1]
        ax.stackplot(x, *[ms[key].to_numpy() for key in order], colors=[COLOR[key] for key in order],
                     linewidth=0)
        for key in stages:
            ax.fill_between([], [], color=COLOR[key], linewidth=0, label=LABEL.get(key, key))
    else:
        for key in stages:
            ax.plot(x, ms[key].to_numpy(), color=COLOR[key], linewidth=1.2,
                    label=LABEL.get(key, key))
    if pf.config.PHASE_TOTAL_LINE:
        ax.plot(x, ms["total"].to_numpy(), color="black", marker="o")
    pf.y_axis(ax, [0.005, 50], figure=FIGURE)
    if linear:
        # A gridline per ns/row, unlabelled; the labels stay at the major ticks.
        ax.yaxis.set_minor_locator(pf.mpl.ticker.MultipleLocator(1))
        ax.tick_params(axis="y", which="minor", length=0)
        ax.grid(axis="y", which="minor", color=pf.mpl.rcParams["grid.color"],
                linewidth=pf.mpl.rcParams["grid.linewidth"])
    ax.set_ylabel("ns/row")
    pf.selectivity_axis(ax)
    # Column wide, so every bracket gets its label.
    brackets = [0, 1e-5, 1e-4, 1e-3, 1e-2, 1e-1, 1]
    pf.share_ticks(ax, brackets, brackets)
    pf.align_column_x(fig, [ax])
    return fig, ax


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    pf.style()
    panels, stems, split_axes, fused_axes = [], [], [], []
    for name, inputs in INPUTS.items():
        split = HERE / inputs["split"] if "split" in inputs else None
        d = per_million_rows(load(inputs["run"], inputs["label"]), split)
        stages = SPLIT_STAGES if split else FUSED_STAGES
        print(f"{name}: {len(d)} needles, mean share of the query's time per bin")
        panels.append(share_panel(d, stages))
        stems.append(pf.slug(name))
        (split_axes if split else fused_axes).append(panels[-1][1])
    pf.clear_figure(FIGURE, args.out)
    for (fig, ax), stem in zip(panels, stems):
        pf.save_panel(fig, FIGURE, stem, args.out)
    pf.save_legend(split_axes, FIGURE, ncol=len(SPLIT_STAGES), out_dir=args.out,
                   width=pf.COLUMN_IN)
    pf.save_legend(fused_axes, FIGURE, ncol=len(FUSED_STAGES), out_dir=args.out,
                   width=pf.COLUMN_IN, stem="legend-unsplit")


if __name__ == "__main__":
    main()
