# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "duckdb>=1.1", "numpy", "pyarrow>=17", "xxhash>=3"]
# ///
"""Throughput over the selected rows against the share of rows an upstream
predicate pre-selected, from a `paper/specs/rows/selection.toml` run:
every system taking the selection, the median over the needles per share, as
the row group panel draws, every share the run holds, with `all` as the top
point. The x axis is rows selected per
row group, a chunk of 0 being the whole column.

The harness reports GB/s over the whole column. The bytes of the selected
rows come from the dataset under the harness's own seeded selection
(harness/src/selection.rs), checked against the row count the run stored.

    uv run paper/plot_selection.py [--run RUN] [--out DIR]

The runs default to config.INPUTS["rows"]["selection"], uncompressed memmem
from its own run there. The pooled
panel the paper includes is drawn by plot_micro.py, as micro-rows/selection,
from load() and panel() here. Writes `micro-selection-all/time.pdf`, the cut scans'
time per query over their planning floor, and `micro-selection/<column>.pdf`,
`micro-selection/legend.pdf` and `micro-selection/planning.pdf`, planning's
share of the cut scans' query time.
"""

import argparse
import re

import numpy as np
import pyarrow.compute as pc
import pyarrow.ipc as ipc
import xxhash

import paperfig as pf
import plot_micro as pm

# The systems of plot_micro.SYSTEMS, with memmem over the uncompressed column
# from the candidate that takes the selection; under `all` it is the same one
# pass over the payload. The decode baselines are dashed, without markers.
SYSTEMS = [
    ("uncompressed_memmem/row-hay" if label == "uncompressed/direct+memmem-hay" else label,
     name, color, marker)
    for label, name, color, marker in pm.SYSTEMS
]


def selected_bytes(dataset, selections):
    """{selection: (rows, bytes)} of the `uniform` selections: row r is selected
    when xxh3(r, seed) >> 11 over 2^53 falls below the fraction."""
    col = ipc.open_file(pm.HERE.parent / "datasets" / dataset / "data.arrow").read_all().column(0)
    lens = pc.binary_length(col).to_numpy(zero_copy_only=False)
    draws, out = {}, {}
    for name in selections:
        f, seed = re.fullmatch(r"uniform-f([0-9.e-]+)-s(\d+)", name).groups()
        seed = int(seed)
        if seed not in draws:
            draws[seed] = np.fromiter(
                ((xxhash.xxh3_64_intdigest(r.to_bytes(8, "little"), seed) >> 11) / float(1 << 53)
                 for r in range(len(lens))), float, len(lens))
        mask = draws[seed] < float(f)
        out[name] = (int(mask.sum()), int(lens[mask].sum()))
    return out, int(lens.sum())


def load(run):
    con = pm.duckdb.connect(str(pm.DB), read_only=True)
    d = con.sql(
        """
        select dataset, label, selection, selection_rows, num_rows, chunk_rows, selectivity, gbps
        from v where run_name = ? and is_latest and status = 'ok'
        """,
        params=[run],
    ).df()
    # `all` is the top point, every row selected; 99% would sit on top of it.
    every = d[d["selection"] == "all"].assign(share=1.0)
    print(every.groupby(["dataset", "label"])["gbps"].median().round(1).unstack().to_string())
    d = d[d["selection"] != "all"].copy()
    for dataset, g in d.groupby("dataset"):
        sizes, payload = selected_bytes(dataset, g["selection"].unique())
        for name, (rows, nbytes) in sizes.items():
            at = (d["dataset"] == dataset) & (d["selection"] == name)
            if not (d.loc[at, "selection_rows"] == rows).all():
                raise SystemExit(f"{dataset} {name}: {rows} rows rebuilt, the run has another count")
            d.loc[at, "gbps"] *= nbytes / payload
    # Each share is drawn at its nominal fraction.
    nominal = d["selection"].str.extract(r"uniform-f([0-9.e-]+)-s")[0].astype(float)
    d = pm.pd.concat([d.assign(share=nominal), every], ignore_index=True)
    # Rows selected per row group; a chunk of 0 is the whole column.
    group = d["chunk_rows"].where(d["chunk_rows"] > 0, d["num_rows"])
    return d.assign(rows=d["share"] * group)


def load_selection():
    """The pinned selection run, uncompressed memmem from its own run."""
    runs = pf.inputs("rows")["selection"]
    d = load(runs["run"])
    d = d[~d["label"].str.startswith("uncompressed_memmem/")]
    return pm.pd.concat([d, load(runs["uncompressed"])], ignore_index=True)


def panel(ax, d):
    """Drawn against the rows selected per row group, the axis micro-rows'
    row group panel shares."""
    for label, name, color, marker in SYSTEMS:
        m = d[d["label"] == label].groupby("rows")["gbps"].median()
        if not m.empty:
            pm.series(ax, label, name, color, marker, m.index, m.values)
    pm.rows_axis(ax, "rows selected")
    # The share of the row group selected, as dotted lines at each decade,
    # labelled on the 100 GB/s line in the tick labels' size.
    group = (d["rows"] / d["share"]).iloc[0]
    for share, text in [(1e-2, r"1\%"), (1e-1, r"10\%")]:
        ax.axvline(share * group, color="#999999", linestyle=":", linewidth=0.8, zorder=0)
        ax.text(share * group, 100, text, ha="center", va="center_baseline",
                fontsize=pf.mpl.rcParams["ytick.labelsize"], color="#555555",
                bbox=dict(facecolor="white", edgecolor="none", pad=0.5))


# The cut scans in symbol-table order: FSST's 255 symbols, OnPair's 2^12 and 2^16 tokens.
CUT_SYSTEMS = ["fsst_prefilter/fsst-prefilter", "onpair_spiral/pf_walk@bits=12,prune=1.0",
               "onpair_spiral/pf_walk@bits=16,prune=1.0"]


def planning(run):
    """Per needle and share, the planning time over the query time. Planning sees
    the needle and the dictionary but not the selection, so its time is the one
    the run's instrumented pass recorded under `all`."""
    con = pm.duckdb.connect(str(pm.DB), read_only=True)
    d = con.sql(
        """
        select s.dataset, s.label, s.selection, a.setup_ns / s.total_ns as share_planning
        from v s join v a using (run_name, dataset, label, query_id, chunk_rows)
        where s.run_name = ? and s.is_latest and s.status = 'ok'
          and a.selection = 'all' and s.selection != 'all' and a.setup_ns is not null
          and s.label in (select unnest(?))
        """,
        params=[run, CUT_SYSTEMS],
    ).df()
    nominal = d["selection"].str.extract(r"uniform-f([0-9.e-]+)-s")[0].astype(float)
    d = d.assign(share=nominal)
    return d.assign(share_planning=d["share_planning"].clip(upper=1.0))


def planning_panel(run, out):
    """`micro-selection/planning`: planning's share of the query time against the
    share of rows pre-selected, one line per cut scan, over every column."""
    d = planning(run)
    print(d.groupby(["dataset", "label", "share"])["share_planning"].median().mul(100).round(0)
          .unstack("share").to_string())
    fig, ax = pf.panel(width=pf.COLUMN_IN)
    for label, name, color, marker in SYSTEMS:
        if label in CUT_SYSTEMS:
            m = d[d["label"] == label].groupby("share")["share_planning"].median() * 100
            ax.plot(m.index, m.values, marker=marker, color=color, label=name)
    share_axis(ax)
    ax.set_ylim(0, 100)
    ax.set_ylabel(r"planning (\% of query)")
    pf.save_panel(fig, "micro-selection", "planning", out)


def time_panel(run, df, out):
    """`micro-selection-all/time`: microseconds per query against the share
    pre-selected for the cut scans, with each one's planning per query dashed.
    Planning sees the needle and the row group's dictionary, not the selection,
    so it is measured where nothing is selected and draws as a flat floor."""
    con = pm.duckdb.connect(str(pm.DB), read_only=True)
    t = con.sql(
        """
        select label, selection, total_ns / 1e3 as us, setup_ns / 1e3 as plan_us
        from v where run_name = ? and is_latest and status = 'ok'
          and label in (select unnest(?))
        """,
        params=[run, CUT_SYSTEMS],
    ).df()
    plan = t[t["selection"] == "all"].groupby("label")["plan_us"].mean()
    t = t[t["selection"] != "all"]
    nominal = t["selection"].str.extract(r"uniform-f([0-9.e-]+)-s")[0].astype(float)
    t = t.assign(share=nominal)
    print("planning per query, us:", plan.round(1).to_dict())
    print(t.groupby(["label", "share"])["us"].mean().round(1).unstack("share").to_string())
    fig, ax = pf.panel(width=pf.COLUMN_IN)
    for label, name, color, marker in SYSTEMS:
        if label in CUT_SYSTEMS:
            m = t[t["label"] == label].groupby("share")["us"].mean()
            ax.plot(m.index, m.values, marker=marker, color=color, label=name)
            ax.axhline(plan[label], color=color, linestyle="--", linewidth=1.0)
    share_axis(ax)
    pf.y_axis(ax)
    ax.set_ylabel(r"$\mu$s per query")
    pf.save_panel(fig, "micro-selection-all", "time", out)


def share_axis(ax):
    pf.selectivity_axis(ax, label="rows pre-selected")
    # No rows selected answers nothing, so the axis starts at the 10^-5 bracket
    # rather than at the zero bracket the other figures keep.
    ax.set_xticks([1e-5, 1e-4, 1e-3, 1e-2, 1e-1, 1])
    ax.set_xticklabels(["", "$10^{-4}$", "", "$10^{-2}$", "", "1"])
    ax.set_xlim(0.87e-5, 1.3)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", default=None)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    run = args.run or pf.inputs("rows")["selection"]["run"]
    df = load(args.run) if args.run else load_selection()
    if df.empty:
        raise SystemExit(f"no rows for {run}")
    med = df.groupby(["dataset", "label", "share"])["gbps"].median()
    print(med.round(1).unstack("share").to_string())
    pooled = df.groupby(["label", "share"])["gbps"].median()
    print(pooled.round(1).unstack("share").to_string())

    pf.clear_figure("micro-selection-all", args.out)
    time_panel(run, df, args.out)

    pf.clear_figure("micro-selection", args.out)
    drawn = [(k, pm.TITLES[k]) for k in pm.DATASET_ORDER if k in set(df["dataset"])]
    width = pm.dataset_width(len(drawn))
    lim = [med.min() * 0.6, med.max() * 1.3]
    panels = []
    for dataset, _title in drawn:
        fig, ax = pf.panel(width=width)
        panel(ax, df[df["dataset"] == dataset])
        ax.set_ylabel("GB/s of selected rows")
        panels.append((fig, ax))
    pm.gbs_axis([ax for _, ax in panels], lim)
    pf.save_row(panels, "micro-selection", [pf.slug(t) for _, t in drawn], width, args.out)
    pf.save_legend(panels[-1][1], "micro-selection", pm.LEGEND_COLS, args.out)
    planning_panel(run, args.out)


if __name__ == "__main__":
    main()
