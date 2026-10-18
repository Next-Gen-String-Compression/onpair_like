# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "duckdb>=1.1", "numpy"]
# ///
"""Which stage-one kernel the planner picks, against needle selectivity: the
share of (row group, needle) scans per kernel, stacked, on NEON. One
column-wide panel, FSST, OnPair12 and OnPair16 as three strips on one
selectivity axis, named only by their y label; the caption is the LaTeX's.

    kernel-choice/neon.pdf
    kernel-choice/legend.pdf

    uv run paper/plot_kernel_choice.py [--log results/plan-log/<tag>-<date>] [--out DIR]

The pick is a function of the cut's shape alone (K points, R ranges, P code
pairs), so it is recomputed from the plan log's cuts with the NEON cost
model rather than logged. COST mirrors the policies as they are now:
onpair's `scan/policy/mod.rs` (`neon`) and the FSST port's
`scan/policy/policy.hpp`. Selectivity is binned in half decades as in plot_plan_cut.py.
"""

import argparse
import math
from pathlib import Path

import paperfig as pf
from config import SCHEME
from plot_micro import selectivity_x
from plot_plan_cut import load

ROOT = Path(__file__).resolve().parent.parent
ALGORITHMS = ["fsst", "onpair12", "onpair16"]

STRIP_IN = 0.587
PER_BATCH = 8
MAX_BATCHES = {"onpair": 16, "fsst": 1}

# ns per code, in each policy's ladder order: a tie goes to the first.
COST = {"onpair": {
    "table": lambda k, r, p, b: 0.189,
    "eq_or": lambda k, r, p, b: 0.00433 + 0.01414 * k + 0.02153 * r,
    "range": lambda k, r, p, b: 0.00350 + 0.02127 * r,
    "nibble_n8k": lambda k, r, p, b: 0.02178 + 0.03216 * b + 0.02211 * r,
}, "fsst": {
    "eq_or": lambda k, r, p, b: 0.00330 + 0.00721 * k + 0.01131 * r + 0.01674 * p,
    "range": lambda k, r, p, b: 0.00350 + 0.01078 * r,
    "nibble_n8k": lambda k, r, p, b: 0.02449 + 0.01230 * r,
    "nibble": lambda k, r, p, b: 0.03860,
}}

# The paper's names. FSST's nibble bit-table and OnPair's byte table are one
# kernel there, a lookup table over the whole code space, and one series here.
# Compare and nibble_8k test a cut's ranges in a second check, so a cut with
# ranges is its own series; the lookup table marks them in the table.
KERNELS = ["eq_or", "eq_or+range", "range", "nibble_n8k+range", "nibble_n8k", "lookup"]
LOOKUP = {"nibble", "table"}
LABEL = {"eq_or": "compare", "eq_or+range": "compare + range", "range": "range",
         "nibble_n8k+range": r"\texttt{nibble\_8k} + range", "nibble_n8k": r"\texttt{nibble\_8k}",
         "lookup": "lookup table"}
COLOR = {"eq_or": pf.COLOR["eq_or"], "eq_or+range": "#56B4E9", "range": pf.COLOR["range"],
         "nibble_n8k+range": pf.COLOR["bitmap"], "nibble_n8k": pf.COLOR["nibble"],
         "lookup": pf.COLOR["table"]}


def series(kernel, ranges):
    if kernel in LOOKUP:
        return "lookup"
    if kernel in ("eq_or", "nibble_n8k") and ranges > 0:
        return f"{kernel}+range"
    return kernel


def takes(family, kernel, k, r, p, b):
    if kernel == "table":
        return True
    if kernel == "eq_or":
        return k > 0 or (family == "fsst" and p > 0)
    if kernel == "range":
        return k == 0 and p == 0 and r > 0
    if kernel == "nibble_n8k":
        return p == 0 and k > 0 and b <= MAX_BATCHES[family]
    if kernel == "nibble":
        return p == 0


def pick(algorithm, k, r, p):
    family = "fsst" if algorithm == "fsst" else "onpair"
    b = math.ceil(k / PER_BATCH)
    ladder = COST[family]
    return min((m for m in ladder if takes(family, m, k, r, p, b)),
               key=lambda m: ladder[m](k, r, p, b))


def shares(d):
    """Share of scans per kernel in each selectivity bin, bins as columns."""
    counts = d.pivot_table(index="sel", columns="kernel", values="uuid", aggfunc="count",
                           fill_value=0)
    return counts.div(counts.sum(axis=1), axis=0).reindex(columns=KERNELS, fill_value=0.0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--log", type=Path, default=None)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    log = args.log or ROOT / pf.inputs("plan-cut")["log"]
    d = selectivity_x(load(log))
    print(f"{log}: {len(d)} (row group, needle, algorithm) cells")

    picked = {algorithm: a.assign(kernel=[series(pick(algorithm, k, r, p), r) for k, r, p
                                          in a[["codes", "ranges", "pairs"]].itertuples(index=False)])
              for algorithm in ALGORITHMS for a in [d[d["algorithm"] == algorithm]]}
    drawn = [m for m in KERNELS if any((p["kernel"] == m).any() for p in picked.values())]

    pf.clear_figure("kernel-choice", args.out)
    pf.style()
    fig, axes = pf.plt.subplots(len(ALGORITHMS), 1, sharex=True, constrained_layout=True,
                                figsize=(pf.COLUMN_IN, STRIP_IN * len(ALGORITHMS)))
    for ax, algorithm in zip(axes, ALGORITHMS):
        s = shares(picked[algorithm])[drawn]
        print(f"\n{algorithm}: share of scans per kernel per selectivity bin")
        print(s.round(3).to_string())
        ax.stackplot(s.index.values, *[s[m].values for m in drawn],
                     colors=[COLOR[m] for m in drawn], labels=[LABEL[m] for m in drawn],
                     linewidth=0)
        pf.selectivity_axis(ax)
        ax.set_ylim(0, 1)
        ax.set_yticks([0, 0.5, 1])
        ax.set_yticklabels(["0", ".5", "1"])
        ax.set_ylabel(SCHEME[algorithm].replace("OnPair", "OP"))
        ax.grid(False)
    for ax in axes[:-1]:
        ax.set_xlabel("")
        ax.tick_params(labelbottom=False)
    fig.align_ylabels(axes)
    pf.align_column_x(fig, list(axes))
    pf.save(fig, "kernel-choice/neon", args.out)
    pf.plt.close(fig)
    pf.save_legend(list(axes), "kernel-choice", ncol=3, out_dir=args.out, width=pf.COLUMN_IN)


if __name__ == "__main__":
    main()
