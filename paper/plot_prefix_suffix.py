# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "duckdb>=1.1", "numpy"]
# ///
"""Throughput against needle selectivity for PREFIX (left) and SUFFIX (right),
from a `paper/specs/prefix-suffix/<size>.toml` run on NEON: the median per
selectivity bracket over the needles of all four columns, at the 512k row group,
one column wide.

    uv run paper/plot_prefix_suffix.py [--run RUN] [--out DIR]

The run defaults to config.INPUTS["prefix-suffix"]["run"]. Writes
`micro-prefix-suffix/prefix.pdf`, `suffix.pdf` and `legend.pdf`, which the
paper includes, and the same pair per column, not in the paper, as
`micro-prefix-suffix-columns/<column>-prefix.pdf` and `<column>-suffix.pdf`.
"""

import argparse

import paperfig as pf
import plot_micro as pm

# The baselines compare each row at its start or end with the memmem scanner,
# memmem-hay being contains-only; each is drawn as the contains figure's series
# of the same name, whose label it is given here.
AS_CONTAINS = {
    "fsst/decode+memmem": "fsst/decode+memmem-hay",
    "onpair/decode+memmem": "onpair/decode+memmem-hay",
    "uncompressed/direct+memmem": "uncompressed/direct+memmem-hay",
}

OPS = ["prefix", "suffix"]


def load(run):
    con = pm.duckdb.connect(str(pm.DB), read_only=True)
    d = con.sql(
        """
        select dataset, label, op, chunk_rows, selectivity, gbps
        from v where run_name = ? and is_latest and status = 'ok'
        """,
        params=[run],
    ).df()
    d = d[d["chunk_rows"] == pm.ROW_GROUP]
    return d.assign(label=d["label"].replace(AS_CONTAINS))


def op_panel(ax, d):
    """Every system's median per selectivity bracket, over every column."""
    d = pm.selectivity_x(d)
    for label, name, color, marker in pm.SYSTEMS:
        m = d[d["label"] == label].groupby("sel")["gbps"].median()
        if not m.empty:
            pm.series(ax, label, name, color, marker, m.index, m.values)
    pf.selectivity_axis(ax)
    ax.set_ylabel("uncompressed GB/s")


def save_pair(d, name, stems, out):
    """The prefix and suffix panels of `d` on one y axis, as `name/<stem>.pdf`."""
    width = 0.48 * pf.COLUMN_IN
    panels = []
    for op in OPS:
        fig, ax = pf.panel(width=width)
        op_panel(ax, d[d["op"] == op])
        panels.append((fig, ax))
    pf.y_axis([ax for _, ax in panels], [0.1, 300.0], [0.01, 0.1, 1, 10, 100, 1000], "{:g}")
    pf.save_row(panels, name, stems, width, out, unit=pf.COLUMN_IN)
    return panels


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", default=None)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    run = args.run or pf.inputs("prefix-suffix")["run"]
    d = load(run)
    if d.empty:
        raise SystemExit(f"no rows for {run}; run paper/gen-prefix-suffix.sh first")
    print(d.groupby(["op", "label"])["gbps"].median().round(1).unstack(0).to_string())

    pf.clear_figure("micro-prefix-suffix", args.out)
    panels = save_pair(d, "micro-prefix-suffix", OPS, args.out)
    pf.save_legend([ax for _, ax in panels], "micro-prefix-suffix", 2, args.out,
                   width=pf.COLUMN_IN)

    pf.clear_figure("micro-prefix-suffix-columns", args.out)
    for dataset in pm.DATASET_ORDER:
        column = pf.slug(pm.TITLES[dataset])
        save_pair(d[d["dataset"] == dataset], "micro-prefix-suffix-columns",
                  [f"{column}-{op}" for op in OPS], args.out)


if __name__ == "__main__":
    main()
