# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2"]
# ///
"""Stage two: both resolvers against codes in the cut per row, the one number the
planner has, with the planner's threshold drawn in.

    uv run paper/plot_resolver.py [--data paper/data/resolver_*.csv] [--out DIR] [--name FIGURE]

The CSV is a `novel_resolve_*.csv` from onpair's `scan::bench::resolver` or
from the FSST port's `prefilter_resolver_sweep`; the latter is drawn with
`--data paper/data/resolver_fsst_<arch>_<date>.csv --name kernel-resolver-fsst`.
"""

import argparse
from pathlib import Path

import numpy as np
import pandas as pd

import paperfig as pf

HERE = Path(__file__).resolve().parent
LABEL = {"linear_seek": "linear seek", "gallop_seek": "gallop seek"}
# Where policy's two seek lines cross, 3.98 + 0.41g against 3.89 log2(1+g):
# the planner gallops below one hit row in 42. There is no constant for it,
# `select_resolver` evaluates both lines at the g it estimates.
THRESHOLD = 1 / 42


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", default=HERE / pf.inputs("kernel")["resolver"])
    ap.add_argument("--out", default=None)
    ap.add_argument("--name", default="kernel-resolver")
    args = ap.parse_args()

    df = pd.read_csv(args.data)
    df = df[(df["density"] > 0) & df["gbs"].notna()]
    df["per_row"] = df["density"] * df["codes_per_row"]

    # Half-decade bins of the x axis; the line is each resolver's median in
    # the bin, the points are the measurements behind it.
    df["bin"] = (np.log10(df["per_row"]) * 2).round() / 2
    fig, ax = pf.figure(height=1.7)
    for name in ["linear_seek", "gallop_seek"]:
        rows = df[df["resolver"] == name]
        ax.scatter(rows["per_row"], rows["gbs"], s=4, alpha=0.25, linewidths=0,
                   marker=pf.MARKER[name], color=pf.COLOR[name])
        med = rows.groupby("bin")["gbs"].median()
        ax.plot(10 ** med.index, med.values, color=pf.COLOR[name], label=LABEL[name])
    ax.axvline(THRESHOLD, color="#707070", linestyle="--", linewidth=0.7)
    ax.set_xscale("log")
    ax.set_yscale("log")
    ax.set_xlabel(r"codes in the cut per row  (hit density $\times$ codes per row)")
    ax.set_ylabel("GB/s")
    top = ax.get_ylim()[1]
    ax.text(THRESHOLD / 1.4, top, "gallop", fontsize=pf.SMALL_PT, color="#505050", ha="right", va="top")
    ax.text(THRESHOLD * 1.4, top, "linear", fontsize=pf.SMALL_PT, color="#505050", ha="left", va="top")
    ax.legend(loc="lower left", handlelength=1.5)
    pf.save(fig, args.name, args.out)


if __name__ == "__main__":
    main()
