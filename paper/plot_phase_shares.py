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
divides its scan. The paper shows OnPair12 on NEON and says the others look
the same; the rest are drawn for the assets only.

Two rows on one selectivity axis. Top: the query's time per million rows of
its column in black, and each phase's in its band's colour, on a log axis, so
that a stage losing share is not read as getting cheaper. Bottom: stacked
bands, each a phase's share of the query's time: the per-query prep, the
minimum cut scan and the alignment walk that verifies the hits. All are
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
    "prep": "#CC79A7",
    "matcher": "#D55E00",
    "resolver": "#009E73",
    "scan": "#D55E00",
    "walk": "#0072B2",
}
LABEL = {"prep": "planning", "walk": "verify", "scan": "matcher + resolver"}
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
    fig, (top, ax) = plt.subplots(2, 1, sharex=True, figsize=(pf.COLUMN_IN, 1.6),
                                  height_ratios=[2, 1], constrained_layout=True)
    x = ms.index.to_numpy()
    for key in stages:
        top.plot(x, ms[key].to_numpy(), color=COLOR[key], linewidth=1.2)
    top.plot(x, ms["total"].to_numpy(), color="black", marker="o")
    pf.y_axis(top, [0.005, 50], figure=FIGURE)
    top.set_ylabel("ms/$10^6$ rows")
    ax.stackplot(bins.index.to_numpy(), *[bins[key].to_numpy() for key in stages],
                 colors=[COLOR[key] for key in stages],
                 labels=[LABEL.get(key, key) for key in stages], linewidth=0)
    pf.selectivity_axis(ax)
    ax.set_ylim(0, 1)
    ax.set_yticks([0, 0.5, 1])
    ax.set_ylabel("share of time")
    ax.grid(False)
    fig.align_ylabels([top, ax])
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
        # The legend strip centres on the plot area, not the panel with its y
        # label, by an \hspace of the ink left of the axes less the ink right.
        fig.canvas.draw()
        inv = fig.dpi_scale_trans.inverted()
        box = ax.get_window_extent().transformed(inv)
        ink = fig.get_tightbbox()
        offset = ((box.x0 - ink.x0) - (ink.x1 - box.x1)) / pf.COLUMN_IN
        print(f"{FIGURE}/{stem}: legend \\hspace{{{offset:.4f}\\columnwidth}}")
        pf.save_panel(fig, FIGURE, stem, args.out)
    pf.save_legend(split_axes, FIGURE, ncol=len(SPLIT_STAGES), out_dir=args.out,
                   width=pf.COLUMN_IN)
    pf.save_legend(fused_axes, FIGURE, ncol=len(FUSED_STAGES), out_dir=args.out,
                   width=pf.COLUMN_IN, stem="legend-unsplit")


if __name__ == "__main__":
    main()
