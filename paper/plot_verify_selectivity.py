# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "numpy>=1.26", "duckdb>=1.1"]
# ///
"""What each phase of a prefiltered query costs as the needle gets less
selective, over every column pooled (`verify-selectivity`).

    uv run paper/plot_verify_selectivity.py [--run NAME] [--bits 12]
                                            [--split CSV] [--out DIR]

Six lines over one axis: the per-query prep, the two halves of the minimum cut
scan, and the three verifiers. The verifiers share a colour and separate by
dash, since the comparison between them is the figure's subject and their
distance from the scan is its context; the caption is where that is said, the
legend being too narrow for it at column width.

Prep and the verifiers are the harness's instrumented phases, one timed pass
each outside the timing loop, divided by the rows of the column. Matcher and
resolver come from --split, a CSV of the two scan stages timed apart; they are
two passes where the query runs one fused loop, so they sit within a fifth of
the scan they decompose rather than partitioning it exactly.

Against the box plot, this trades the per-candidate-row constant, which
selectivity cannot move, for the cost per row of column, which is what the
query pays. Bins are half a decade, as in plot_micro.py, and the line is their
median.

The axis is symlog, as plot_micro.py's selectivity axes are: a needle that
matches nothing has a selectivity of 0, which a log axis cannot place, and
parking it at a stand-in below the range opens a gap no data can fall into.
Linear up to one row of 2^20, the smallest a column can express, and
logarithmic above.
"""

import argparse
from pathlib import Path

import duckdb
import numpy as np
import pandas as pd

import paperfig as pf

HERE = Path(__file__).resolve().parent
DB = HERE.parent / "results" / "bench.duckdb"
INPUTS = pf.inputs("verify")
# The row group paper/specs/verify-1mi.toml measures at, and the one every
# micro figure but the sweep is read at.
ROW_GROUP = 524288

# The three verifiers are one colour and three dashes: a reader compares them
# with each other, not with the two scan stages they sit on top of.
VERIFY = "#0072B2"
VERIFIERS = [
    ("pf_walk", "walk", "walk", "-", "o"),
    ("pf_memmem_superset", "memmem", "decompress", "--", "^"),
    ("pf_kmp_superset", "kmp", "KMP", ":", "s"),
]
STAGES = [
    ("prep", "prep", "#CC79A7", "-", "v"),
    ("matcher", "matcher", "#D55E00", "-", "D"),
    ("resolver", "resolver", "#009E73", "-", "P"),
]


def load(run, bits):
    con = duckdb.connect(str(DB), read_only=True)
    return con.sql(
        """
        select strategy, dataset, query_id, selectivity, num_rows,
               total_ns, coalesce(setup_ns, 0) as setup_ns,
               coalesce(verify_ns, 0) as verify_ns
        from v
        where run_name = ? and is_latest and candidate = 'onpair_spiral'
          and status = 'ok' and chunk_rows = ? and config like ?
        """,
        params=[run, ROW_GROUP, '{"bits":%d%%' % bits],
    ).df()


def per_million_rows(df, split_csv):
    """One row per needle, every phase in ms per million rows of its column."""
    by = {s: g.set_index("query_id") for s, g in df.groupby("strategy")}
    for strategy, _, _, _, _ in VERIFIERS:
        if strategy not in by:
            raise SystemExit(
                f"the run has no {strategy} rows: this figure needs all three "
                "verifiers, which paper/specs/verify-1mi.toml runs."
            )
    walk = by["pf_walk"]
    unit = walk["num_rows"] / 1e6
    out = pd.DataFrame(index=walk.index)
    out["selectivity"] = walk["selectivity"]
    out["prep"] = walk["setup_ns"] / 1e6 / unit
    for strategy, key, _, _, _ in VERIFIERS:
        ns = by[strategy]["verify_ns"].reindex(walk.index)
        out[key] = (ns / 1e6 / unit).where(ns > 0)

    split = pd.read_csv(split_csv).set_index("query_id")
    missing = walk.index.difference(split.index)
    if len(missing):
        raise SystemExit(f"{len(missing)} needles are not in {split_csv}")
    for stage in ("matcher", "resolver"):
        out[stage] = split[f"{stage}_ns"].reindex(walk.index) / 1e6 / unit

    # Binned where there is a selectivity to take a logarithm of; the needles
    # that match nothing bin at a true 0, which the symlog axis can place.
    live = out["selectivity"] > 0
    out["x"] = 0.0
    out.loc[live, "x"] = 10 ** ((np.log10(out.loc[live, "selectivity"]) * 2).round() / 2)
    return out


def series(ax, d, column, color, label, linestyle, marker):
    med = d[["x", column]].dropna().groupby("x")[column].median()
    ax.plot(med.index.to_numpy(), med.to_numpy(), color=color, label=label,
            linestyle=linestyle, marker=marker)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", default=INPUTS["run"])
    ap.add_argument("--bits", type=int, default=12)
    ap.add_argument("--split", default=str(HERE / INPUTS["split"]))
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    d = per_million_rows(load(args.run, args.bits), args.split)
    zero = d[d["selectivity"] <= 0]
    print(f"{len(d)} needles, {len(zero)} of them matching nothing")
    print("all:", d.drop(columns=["x", "selectivity"]).median().round(3).to_dict())
    print("no match:", zero.drop(columns=["x", "selectivity"]).median().round(4).to_dict())

    pf.style()
    fig, ax = pf.panel(width=pf.COLUMN_IN, height=1.95)
    for column, label, color, linestyle, marker in STAGES:
        series(ax, d, column, color, label, linestyle, marker)
    for _, key, label, linestyle, marker in VERIFIERS:
        series(ax, d, key, VERIFY, label, linestyle, marker)

    ax.set_yscale("log")
    pf.selectivity_axis(ax)
    ax.set_ylabel("ms per million rows")
    # Above the panel, where 6_evaluation.tex puts every legend strip.
    ax.legend(loc="lower center", bbox_to_anchor=(0.5, 1.0), ncol=3,
              handlelength=1.6, columnspacing=1.0, handletextpad=0.4,
              borderpad=0, borderaxespad=0.0)
    pf.save(fig, "verify-selectivity", args.out)


if __name__ == "__main__":
    main()
