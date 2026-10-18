# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "duckdb>=1.1", "numpy", "pyarrow>=17", "xxhash>=3"]
# ///
"""Micro-benchmark figures from `paper/specs/micro/*.toml` runs: our prefilter
at 12 and 16 dictionary bits against decompress-then-memmem, plaintext memmem
and the FSST-LIKE interpreter, on every machine given.

    uv run paper/plot_micro.py [--run "ARM, NEON=<run>" --run ...] [--out DIR]

The runs default to config.INPUTS["micro"]; config.LINEAR_Y draws the y axes linear.

One PDF per panel, the grid assembled in LaTeX with subfigure. A panel is one
(dataset, machine) pair, numbered across then down, beside a legend strip:
`<dimension>_1.pdf` ... `<dimension>_6.pdf` and `<dimension>_legend.pdf`. The
dimensions are:

    micro-selectivity  GB/s against needle selectivity, at a 512k-row row group
    micro-needle       GB/s against needle length, at a 512k-row row group
    micro-rowgroup     GB/s against row group size

A machine with no run gets an empty row of panels. The compression and
throughput tables are printed per machine.
"""

import argparse
from pathlib import Path

import duckdb
import numpy as np
import pandas as pd

import config
import paperfig as pf
from config import name

HERE = Path(__file__).resolve().parent
DB = HERE.parent / "results" / "bench.duckdb"

# Panel titles carry both the column and the machine, so both names are short
# enough for a third of the text width. Every column the micro specs measure, in
# panel order.
TITLES = {"imdb-charname-1mi": "Character names", "amazon-title-1mi": "Book titles",
          "clickbench-url-1mi": "URL", "dbpedia-abstract-1mi": "DBpedia abstracts"}
DATASET_ORDER = list(TITLES)
# The instruction sets `micro-arch` draws.
ARCH_MACHINES = ["ARM, NEON", "x86, AVX-512"]
# The machine `micro-datasets` and `micro-matrix` draw.
FIGURE_MACHINE = "ARM, NEON"
# Each series is the compression scheme it stores and the way it searches:
# "automaton" is Pop et al.'s compression-aware LIKE over FSST.
# Legend order, which is also the draw order: the legend is laid out in four
# columns of two, and matplotlib fills a column at a time, so the pairs here
# are the pairs on the page.
SYSTEMS = [
    ("simd_cut_onpair/simd-cut@bits=12,prune=1.0", name("onpair12", "cut"), config.COLOR["onpair12-cut"], "s"),
    ("simd_cut_onpair/simd-cut@bits=16,prune=1.0", name("onpair16", "cut"), config.COLOR["onpair16-cut"], "o"),
    # The same planner and walk over an FSST symbol table.
    ("simd_cut_fsst/simd-cut", name("fsst", "cut"), config.COLOR["fsst-cut"], "^"),
    ("fsst_like_tum/interp", name("fsst", "automaton"), config.COLOR["fsst-automaton"], "D"),
    # The same automaton compiled by LLVM per row group, x86 only; the compile
    # is in the query time.
    ("fsst_like_tum/llvm-simd", name("fsst", "compiled"), config.COLOR["fsst-compiled"], "d"),
    ("fsst/decode+memmem-hay", name("fsst", "decode"), config.COLOR["fsst-decode"], "v"),
    ("simd_cut_onpair/decode+memmem-hay@bits=16,prune=1.0", name("onpair16", "decode"), config.COLOR["onpair-decode"], "^"),
    ("uncompressed/direct+memmem-hay", name("uncompressed", "memmem"), config.COLOR["uncompressed"], "x"),
]
LEGEND_COLS = 4
# The legend of micro-datasets and micro-arch: one titled block per method,
# its schemes side by side under it.
LEGEND_GROUPS = [
    ("SIMD-cut", [(name("onpair12", "cut"), config.SCHEME["onpair12"]), (name("onpair16", "cut"), config.SCHEME["onpair16"]),
                  (name("fsst", "cut"), "FSST")]),
    ("Automaton (FSST)", [(name("fsst", "automaton"), "interpreted"),
                          (name("fsst", "compiled"), "compiled")]),
    (r"Decompress \& search", [(name("onpair16", "decode"), config.SCHEME["onpair16"]),
                               (name("fsst", "decode"), "FSST")]),
    ("Search", [(name("uncompressed", "memmem"), "Uncompressed"),
                ("Memory bandwidth", "Memory bandwidth")]),
]
# The decode baselines and uncompressed memmem are dashed, without markers,
# quartile band or per-query points: the level the compressed-domain series is
# read against. row-hay is uncompressed memmem under a pre-selection.
BASELINES = {
    "simd_cut_onpair/decode+memmem-hay@bits=16,prune=1.0": (0, (1, 1.2)),
    "fsst/decode+memmem-hay": (0, (3, 1, 1, 1)),
    "uncompressed/direct+memmem-hay": (0, (4, 1.5)),
    "uncompressed_memmem/row-hay": (0, (4, 1.5)),
}


def series(ax, label, name, color, marker, x, y):
    """A baseline is dashed, in its own pattern, and without markers."""
    if label in BASELINES:
        ax.plot(x, y, color=color, linestyle=BASELINES[label], linewidth=1.0, label=name)
        return
    ax.plot(x, y, marker=marker, color=color, label=name)
# The row group every figure but the sweep is measured at: the largest size a
# format would actually use, rather than the whole column, which gives the
# dictionary the most text it will ever see.
ROW_GROUP = 524288


def load(run):
    """One run's rows."""
    con = duckdb.connect(str(DB), read_only=True)
    return con.sql(
        """
        select dataset, label, chunk_rows, needle_len, selectivity, match_count, prune_rate,
               gbps, compression_ratio,
               num_rows, payload_mb, mean_len,
               total_ns, setup_ns, decode_ns, scan_ns, prefilter_ns, verify_ns
        from v where run_name = ? and is_latest and status = 'ok'
        """,
        params=[run],
    ).df()


# The throughput range every GB/s panel but `micro-rows` shares, so the
# panels compare; main() replaces it with the range the loaded runs cover.
GBS_LIM = [0.1, 300.0]


def gbs_axis(axes, lim=GBS_LIM, figure=None):
    """One panel, or a row of panels that share the axis. The first n panels
    of a figure in config.Y_TOP share its fixed top instead."""
    axes = [axes] if isinstance(axes, pf.mpl.axes.Axes) else list(axes)
    n, top = config.Y_TOP.get(figure, (0, None))
    for group, t in ((axes[:n], top), (axes[n:], None)):
        if group:
            pf.y_axis(group, lim, [0.01, 0.1, 1, 10, 100, 1000], "{:g}", figure=figure, top=t)


def chunk_label(n):
    """2048 -> 2k, 32768 -> 32k: a row group size is a power of two, so its k
    is 1024. The last tick is the column's own row count, which is not a power
    of two, so it reads in thousands: 1000000 -> 1m, 901343 -> 901k."""
    if n & (n - 1) == 0 and n < 1_000_000:
        return f"{n // 1024}k"
    if n >= 1_000_000:
        return f"{n / 1_000_000:g}m"
    return f"{round(n / 1000):g}k"


def chunk_axis(ax, d, rows):
    chunks = sorted(d["chunk"].unique())
    ax.set_xscale("log", base=2)
    ax.set_xticks(chunks)
    ax.set_xticklabels([chunk_label(c) for c in chunks])
    ax.set_xlabel("row group size")


# The x axis the two `micro-rows` panels share: the rows a query scans
# under one symbol table, a small row group's rows or a 512k row group's
# selected ones.
ROWS_TICKS = [2048, 8192, 32768, 131072, 524288]
# Their shared GB/s range, covering both figures' medians and bands.
ROWS_GBS_LIM = [0.1, 300.0]


def rows_axis(ax, label):
    ax.set_xscale("log", base=2)
    ax.set_xticks(ROWS_TICKS)
    ax.set_xticklabels([str(t) if t < 1024 else chunk_label(t) for t in ROWS_TICKS])
    ax.xaxis.set_minor_locator(pf.mpl.ticker.NullLocator())
    ax.set_xlim(1500, 1.6e6)
    ax.set_xlabel(label)


def selectivity_panel(ax, d):
    """Every query as a point, the median of each selectivity bracket as the line."""
    d = selectivity_x(d[d["chunk_rows"] == ROW_GROUP])
    for label, name, color, marker in SYSTEMS:
        g = d[d["label"] == label]
        if label not in BASELINES:
            ax.scatter(g["selectivity"], g["gbps"], s=4, alpha=0.25, linewidths=0,
                       color=color, marker=marker)
        med = g.groupby("sel")["gbps"].median()
        series(ax, label, name, color, marker, med.index, med.values)
    pf.selectivity_axis(ax)
    gbs_axis(ax)


def needle_panel(ax, d):
    d = d[d["chunk_rows"] == ROW_GROUP]
    for label, name, color, marker in SYSTEMS:
        med = d[d["label"] == label].groupby("needle_len")["gbps"].median()
        series(ax, label, name, color, marker, med.index, med.values)
    ax.set_xscale("log", base=2)
    ax.set_xticks([1, 2, 4, 8, 16, 32, 64])
    ax.set_xticklabels(["1", "2", "4", "8", "16", "32", "64"])
    ax.set_xlabel("needle length (bytes)")
    gbs_axis(ax)


def rowgroup_panel(ax, d):
    rows = int(d["num_rows"].iloc[0])
    d = d.assign(chunk=d["chunk_rows"].where(d["chunk_rows"] > 0, rows))
    for label, name, color, marker in SYSTEMS:
        g = d[d["label"] == label].groupby("chunk")["gbps"].median()
        series(ax, label, name, color, marker, g.index, g.values)
    chunk_axis(ax, d, rows)
    gbs_axis(ax)


# ----------------------------------------------------------- collapsed views
# The three dimensions pooled over every column and machine: one line per
# system, the band its middle half. A dataset and a machine shift the rate but
# not the shape, which `micro-datasets` and `micro-arch` are there to show.


def pooled(df, x):
    """Median and quartiles of `gbps` per (system, x), over everything else."""
    g = df.groupby(["label", x])["gbps"]
    return g.median(), g.quantile(0.25), g.quantile(0.75)


def trend_panel(ax, df, x, systems=SYSTEMS, band=True):
    """The caller sets the y axis."""
    df = df[df["label"].isin([s[0] for s in systems])]
    med, lo, hi = pooled(df, x)
    for label, name, color, marker in systems:
        if label not in med.index.get_level_values(0):
            continue
        m = med.loc[label]
        if band and label not in BASELINES:
            ax.fill_between(m.index, lo.loc[label].values, hi.loc[label].values,
                            color=color, alpha=0.15, linewidth=0)
        series(ax, label, name, color, marker, m.index, m.values)


# The needle length buckets the matrix draws: a column per power of two, with
# 1 and 2 bytes apart from 3-4. Needles past 64 bytes are left out.
MATRIX_LEN_EDGES = [1, 2, 4, 8, 16, 32, 64]
MATRIX_LEN_LABELS = ["1", "2", "3-4", "5-8", "9-16", "17-32", "33-64"]


def selectivity_x(df):
    return df.assign(sel=pf.selectivity_bracket(df["selectivity"]))


def drawn_values(loaded):
    """Every y the shared GB/s panels actually draw: the per-column medians
    on FIGURE_MACHINE and the per-machine medians. The axis is fitted to these rather than to single
    queries, which run far below any median and would otherwise leave the
    panels mostly empty."""
    labels = [s[0] for s in SYSTEMS]
    runs = [d[(d["chunk_rows"] == ROW_GROUP) & d["label"].isin(labels)]
            for _, d in loaded if d is not None]
    at = dict(loaded)[FIGURE_MACHINE]
    at = at[(at["chunk_rows"] == ROW_GROUP) & at["label"].isin(labels)]
    vals = []
    for dataset, _title in datasets_of(loaded):
        vals.append(selectivity_x(at[at["dataset"] == dataset])
                    .groupby(["label", "sel"])["gbps"].median())
    for d in runs:
        vals.append(selectivity_x(d).groupby(["label", "sel"])["gbps"].median())
    v = pd.concat(vals).dropna()
    return v.min(), v.max()


def datasets_of(loaded):
    """The columns present in the loaded runs, in panel order."""
    seen = set()
    for _machine, d in loaded:
        if d is not None:
            seen |= set(d["dataset"].unique())
    return [(k, TITLES[k]) for k in DATASET_ORDER if k in seen]


def dataset_width(n):
    """A panel is drawn at the width it prints at; four to a row fill the
    text width edge to edge, the gap between them save_row's, three 0.32, two 0.49."""
    return {2: pf.PANEL_2UP, 4: pf.TEXT_IN / 4}.get(n, pf.PANEL_3UP)


# One panel per system, drawn on one colour scale so the cells compare across
# panels. The minimum cut scan is the subject; decompress-then-memmem is the
# control, since the same cell there is the same needle on the same rows with
# the length effect removed.
# Slowest average cell first, as the paper orders them: the first panel
# carries the row labels. FSST's minimum cut scan is drawn but not in the paper.
MATRIX_SYSTEMS = [("fsst_like_tum/interp", "fsst-automaton"),
                  ("simd_cut_onpair/decode+memmem-hay@bits=16,prune=1.0", "onpair-decode"),
                  ("uncompressed/direct+memmem-hay", "uncompressed"),
                  ("simd_cut_onpair/simd-cut@bits=16,prune=1.0", "onpair16-cut"),
                  ("simd_cut_fsst/simd-cut", "fsst-cut")]
# Rows are decades of selectivity under a zero-match row; columns are the
# needle panel's buckets.
MATRIX_SEL_EDGES = [1e-5, 1e-4, 1e-3, 1e-2, 1e-1]
MATRIX_SEL_LABELS = [r"0\%", r"$<$0.001\%", r"0.001--0.01\%", r"0.01--0.1\%",
                     r"0.1--1\%", r"1--10\%", r"10--100\%"]
MATRIX_EMPTY = ("#EDEBE6", "white")
MATRIX_DATASETS = ("clickbench-url-1mi",)
# A cell holds up to four characters; the tick size crowds them.
MATRIX_CELL_PT = pf.SMALL_PT - 1


MATRIX_CMAP = pf.mpl.colors.ListedColormap(
    pf.mpl.colormaps["YlOrRd"](np.linspace(0.12, 1.0, 256)))


def matrix_cells(d):
    """`d` with the matrix `row` and `col` of each query, needles past the
    last length bucket dropped."""
    d = d[d["needle_len"] <= MATRIX_LEN_EDGES[-1]]
    live = d["selectivity"] > 0
    row = np.where(live, 1 + np.searchsorted(MATRIX_SEL_EDGES, d["selectivity"], side="right"), 0)
    return d.assign(row=row, col=np.searchsorted(MATRIX_LEN_EDGES, d["needle_len"]))


def matrix_pivot(d, values, aggfunc):
    grid = matrix_cells(d).pivot_table(index="row", columns="col", values=values,
                                       aggfunc=aggfunc)
    return grid.reindex(index=range(len(MATRIX_SEL_LABELS)),
                        columns=range(len(MATRIX_LEN_EDGES)))


def matrix_grid(df, label):
    """Median GB/s per (selectivity bucket, needle length bucket) for one
    system."""
    d = df[df["label"] == label]
    return None if d.empty else matrix_pivot(d, "gbps", "median")


def cell_text(v):
    return f"{v:.0f}" if v >= 10 else f"{v:.1f}".removesuffix(".0")


def draw_matrix(fig, ax, grid, norm, cmap=MATRIX_CMAP):
    """One matrix on `ax`: a square per cell, coloured on `norm` or empty, its
    value in bold, a white line on every cell boundary."""
    nrows, ncols = grid.shape
    ax.set_xlim(-0.5, ncols - 0.5)
    ax.set_ylim(nrows - 0.5, -0.5)
    ax.set_aspect("equal")
    # Digits have no descent, so centring the text box sets them high:
    # put the baseline half a digit height below the cell centre instead.
    digit = pf.mpl.textpath.TextPath((0, 0), r"\textbf{0}", size=MATRIX_CELL_PT,
                                     usetex=True).get_extents().y1
    centred = pf.mpl.transforms.offset_copy(ax.transData, fig, y=-digit / 2, units="points")
    for i in range(nrows):
        for j in range(ncols):
            v = grid.values[i, j]
            face = MATRIX_EMPTY[0] if np.isnan(v) else cmap(norm(v))
            ax.add_patch(pf.mpl.patches.Rectangle(
                (j - 0.5, i - 0.5), 1, 1, facecolor=face, linewidth=0))
            if np.isnan(v):
                continue
            r, g, b, _ = face
            dark = 0.2126 * r + 0.7152 * g + 0.0722 * b < 0.5
            ax.text(j, i, r"\textbf{%s}" % cell_text(v), ha="center", va="baseline",
                    transform=centred, fontsize=MATRIX_CELL_PT,
                    color="white" if dark else "#222222")
    # One line per cell boundary, the outer ones included and left
    # unclipped, so the border is the same width everywhere.
    ax.hlines(np.arange(nrows + 1) - 0.5, -0.5, ncols - 0.5, color=MATRIX_EMPTY[1],
              linewidth=1.0, clip_on=False)
    ax.vlines(np.arange(ncols + 1) - 0.5, -0.5, nrows - 0.5, color=MATRIX_EMPTY[1],
              linewidth=1.0, clip_on=False)
    ax.grid(False)
    for spine in ax.spines.values():
        spine.set_visible(False)
    ax.tick_params(length=0, pad=2)
    ax.set_xticks(range(ncols), MATRIX_LEN_LABELS, rotation=45, ha="right",
                  rotation_mode="anchor")
    ax.set_yticks(range(nrows), MATRIX_SEL_LABELS)
    ax.set_xlabel("needle length (bytes)")
    ax.set_ylabel("rows matching")


def matrix_colorbar(bar, norm, axis):
    """A matrix colour scale: 1-2-5 ticks, no outline, no tick marks."""
    ticks = [t for t in (1, 2, 5, 10, 20, 50, 100, 200, 500, 1000)
             if norm.vmin <= t <= norm.vmax]
    bar.set_ticks(ticks, labels=[str(t) for t in ticks])
    axis.set_minor_locator(pf.mpl.ticker.NullLocator())
    bar.outline.set_visible(False)
    bar.ax.tick_params(length=0, pad=2)


def matrix_legend(norm, cmap, out, name="micro-matrix", label="GB/s", label_above=False):
    """The shared colour scale as `<name>/legend.pdf`, cropped to the bar
    and its labels so LaTeX includes it at its natural size. Its label sits
    right of the bar, or centred above it."""
    pf.style()
    fig = pf.plt.figure(figsize=(pf.TEXT_IN, 0.4))
    cax = fig.add_axes([0.3, 0.55, 0.36, 0.18])
    bar = fig.colorbar(pf.mpl.cm.ScalarMappable(norm=norm, cmap=cmap), cax=cax,
                       orientation="horizontal")
    matrix_colorbar(bar, norm, bar.ax.xaxis)
    if label_above:
        cax.text(0.5, 1.25, label, transform=cax.transAxes, ha="center", va="bottom",
                 fontsize=pf.SMALL_PT)
    else:
        cax.text(1.02, 0.5, label, transform=cax.transAxes, va="center",
                 fontsize=pf.SMALL_PT)
    pf.save(fig, f"{name}/legend", out, bbox_inches="tight")
    pf.plt.close(fig)


def matrix_panel(loaded, out):
    """`micro-matrix/<system>`: mean GB/s per (selectivity, needle length)
    cell on ClickBench URLs on FIGURE_MACHINE, one panel per system on a shared colour scale, which is
    `micro-matrix/legend`. The trend panels pool one axis over the other,
    which cannot separate "long needles are rare" from "long needles scan
    faster"; reading down a column of this one holds needle length fixed."""
    pf.clear_figure("micro-matrix", out)
    df = dict(loaded)[FIGURE_MACHINE]
    df = df[(df["chunk_rows"] == ROW_GROUP) & df["dataset"].isin(MATRIX_DATASETS)]
    grids = [(name, matrix_grid(df, label)) for label, name in MATRIX_SYSTEMS]
    grids = [(name, g) for name, g in grids if g is not None]
    if not grids:
        print("micro-matrix: none of its systems ran, skipped")
        return
    drawn = np.concatenate([g.values[~np.isnan(g.values)] for _, g in grids])
    norm = pf.mpl.colors.LogNorm(vmin=max(1.0, drawn.min()), vmax=drawn.max())

    panels = []
    for name, grid in grids:
        fig, ax = pf.panel(height=1.95)
        draw_matrix(fig, ax, grid, norm)
        if panels:
            ax.set_yticklabels([])
        panels.append((fig, ax))
    pf.save_row(panels, "micro-matrix", [name for name, _ in grids], 0.891 * pf.PANEL_4UP, out, gap=0)
    matrix_legend(norm, MATRIX_CMAP, out)


def facet_panel(ax, df):
    """Every system against selectivity, medians per selectivity bracket."""
    d = selectivity_x(df)
    for label, name, color, marker in SYSTEMS:
        m = d[d["label"] == label].groupby("sel")["gbps"].median()
        if m.empty:
            continue
        series(ax, label, name, color, marker, m.index, m.values)
    pf.selectivity_axis(ax)
    ax.set_ylabel("uncompressed GB/s")


# `micro-datasets` marks where this series' median first falls below this baseline's.
CROSS_SERIES = "simd_cut_onpair/simd-cut@bits=16,prune=1.0"
CROSS_BASELINE = "uncompressed/direct+memmem-hay"


def crossing_line(ax, df):
    """A dotted line across the panel at the crossing, interpolated on the
    drawn (symlog, log) axes so it meets the lines where they cross. Call
    after the axes are set."""
    m = selectivity_x(df).groupby(["label", "sel"])["gbps"].median()
    cut, base = m.loc[CROSS_SERIES], m.loc[CROSS_BASELINE].reindex(m.loc[CROSS_SERIES].index)
    below = (cut < base).to_numpy()
    if not below.any() or below[0]:
        return
    i = below.argmax()
    tx = ax.xaxis.get_transform()
    x0, x1 = tx.transform(np.reshape(cut.index[i - 1:i + 1], (-1, 1))).ravel()
    g0, g1 = np.log(cut.iloc[i - 1] / base.iloc[i - 1]), np.log(cut.iloc[i] / base.iloc[i])
    t = g0 / (g0 - g1)
    x = tx.inverted().transform([[x0 + t * (x1 - x0)]])[0, 0]
    ax.axvline(x, color="#222222", linestyle=":", linewidth=0.8, zorder=1)


def memory_line(ax, machine, label=None):
    """One core's read rate from memory on `machine`, "ARM, NEON" and the like."""
    pf.bandwidth_line(ax, pf.read_bandwidth(machine.split(", ")[1]), label)


def dataset_panels(loaded, out):
    """`micro-datasets/1..3`: one column per panel, every system, on
    FIGURE_MACHINE."""
    pf.clear_figure("micro-datasets", out)
    df = dict(loaded)[FIGURE_MACHINE]
    df = df[df["chunk_rows"] == ROW_GROUP]
    drawn = datasets_of(loaded)
    width = dataset_width(len(drawn))
    panels = []
    for dataset, _title in drawn:
        fig, ax = pf.panel(width=width)
        facet_panel(ax, df[df["dataset"] == dataset])
        panels.append((fig, ax))
    gbs_axis([ax for _, ax in panels], figure="micro-datasets")
    if pf.inputs("micro")["bandwidth_line"]:
        for _, ax in panels:
            memory_line(ax, FIGURE_MACHINE, "Memory bandwidth")
    if config.CROSSING_LINE:
        for (_, ax), (dataset, _title) in zip(panels, drawn):
            crossing_line(ax, df[df["dataset"] == dataset])
    pf.save_row(panels, "micro-datasets", [pf.slug(t) for _, t in drawn], width, out)
    pf.save_grouped_legend(panels[-1][1], LEGEND_GROUPS, "micro-datasets", out)


def machine_panels(loaded, out):
    """`micro-arch/<set>`: one instruction set per panel, every system, pooled
    over the columns, two panels in one column of the page, no legend.
    `micro-rows/<panel>`: fewer rows per symbol table on AVX-512, from a
    smaller row group ([rowgroup]) or from a pre-selection of the whole column
    ([selection]) in runs of 1-16 rows or uniform rows, three panels with
    `micro-rows/legend`; plot_no_sort.py's panel completes the row."""
    import plot_selection as ps

    pf.clear_figure("micro-arch", out)
    width = 0.5 * pf.COLUMN_IN
    panels, machines = [], []
    for machine, df in loaded:
        if df is None or machine not in ARCH_MACHINES:
            continue
        fig, ax = pf.panel(width=width)
        facet_panel(ax, df[df["chunk_rows"] == ROW_GROUP])
        panels.append((fig, ax))
        machines.append(machine)
    if not panels:
        return
    gbs_axis([ax for _, ax in panels], figure="micro-arch")
    # One shared range, so only the first panel is numbered.
    for _, ax in panels[1:]:
        ax.set_yticklabels([])
    # No legend entry: the caption names the line.
    if pf.inputs("micro")["bandwidth_line"]:
        for (_, ax), machine in zip(panels, machines):
            memory_line(ax, machine)
    pf.save_row(panels, "micro-arch", [pf.slug(m) for m in machines], width, out, unit=pf.COLUMN_IN)

    # The row group pair, NEON then AVX-512 as in micro-arch, one column wide;
    # the compiled automaton is x86 only.
    rows = pf.inputs("rows")
    groups = []
    for runs in ([rows["rowgroup-neon"]["run"]], [rows["rowgroup"]["run"], rows["rowgroup-llvm"]["run"]]):
        rg = pd.concat([load(run) for run in runs], ignore_index=True)
        rg = rg.assign(chunk=rg["chunk_rows"].where(rg["chunk_rows"] > 0, rg["num_rows"]))
        p = pf.panel(width=pf.COLUMN_IN / 2)
        trend_panel(p[1], rg, "chunk", band=False)
        rows_axis(p[1], "rows per row group")
        groups.append(p)
    # The selection pair fills one column beside micro-arch.
    sel = ps.load_selection()
    pair_width = pf.COLUMN_IN / len(ps.SHAPES)
    panels = []
    for shape in ps.SHAPES:
        p = pf.panel(width=pair_width)
        ps.panel(p[1], sel[(sel["shape"] == shape) | (sel["selection"] == "all")])
        panels.append(p)
    gbs_axis([ax for _, ax in groups], ROWS_GBS_LIM, "micro-trends")
    gbs_axis([ax for _, ax in panels], ROWS_GBS_LIM, "micro-rows")
    for _, ax in groups + panels:
        ax.set_ylabel("uncompressed GB/s")
    # Each pair shares one range, so only its first panel is numbered.
    for _, ax in groups[1:] + panels[1:]:
        ax.set_yticklabels([])
    pf.clear_figure("micro-rows", out)
    pf.save_row(groups, "micro-rows", ["rowgroup-neon", "rowgroup-avx512"], pf.COLUMN_IN / 2, out,
                unit=pf.COLUMN_IN)
    pf.save_row(panels, "micro-rows", ps.SHAPES, pair_width, out, unit=pf.COLUMN_IN)
    pf.save_grouped_legend(groups[1][1], LEGEND_GROUPS, "micro-rows", out)


# The systems with a filter whose false positives `false-positives` draws.
CUT_SCANS = [s for s in SYSTEMS if s[1].endswith(config.METHOD["cut"])]


def fp_panel(loaded, out):
    """`false-positives`: rows passing the cut per row matching the needle,
    median per selectivity bracket, against the bracket's median share of
    matching rows, on FIGURE_MACHINE at a 512k row group. 1x is a perfect
    filter. The rarest bracket is labelled with the share of all rows that
    still pass. Needles that match no row are left out. The run stores the
    prune rate, so candidates are (1 - prune_rate) rows."""
    df = dict(loaded)[FIGURE_MACHINE]
    d = df[(df["chunk_rows"] == ROW_GROUP) & (df["selectivity"] > 0)]
    d = selectivity_x(d.assign(cand=1 - d["prune_rate"]))
    d = d.assign(ratio=d["cand"] / d["selectivity"])
    fig, ax = pf.panel(width=pf.COLUMN_IN, height=1.4)
    hi, xs = 1.0, []
    for label, name, color, marker in CUT_SCANS:
        m = d[d["label"] == label].groupby("sel")[["selectivity", "cand", "ratio"]].median()
        print(f"false-positives {name}:", m.round(6).to_dict("index"))
        ax.plot(m["selectivity"], m["ratio"], marker=marker, color=color, label=name.split(",")[0])
        first = m.iloc[0]
        ax.annotate(f"{100 * first['cand']:.2f}\\% of rows", (first["selectivity"], first["ratio"]),
                    xytext=(5, 0), textcoords="offset points", ha="left", va="center",
                    color=color, fontsize=pf.SMALL_PT, zorder=4,
                    bbox=dict(facecolor="white", edgecolor="none", pad=0.3))
        hi = max(hi, m["ratio"].max())
        xs.extend(m["selectivity"])
    ax.axhline(1, color="#999999", linewidth=0.8, zorder=0)
    ax.set_xscale("log")
    pf.share_ticks(ax, [1e-6, 1e-5, 1e-4, 1e-3, 1e-2, 1e-1, 1], [1e-6, 1e-4, 1e-2, 1])
    ax.xaxis.set_minor_locator(pf.mpl.ticker.NullLocator())
    # Just enough room that the end markers are not cut by the axes.
    ax.set_xlim(min(xs) / 1.25, max(xs) * 1.25)
    ax.set_xlabel("share of rows matching the needle")
    pf.y_axis(ax, [0.7, hi * 2], [1, 10, 100, 1000, 10000], "{:g}$\\times$", figure="false-positives")
    ax.set_ylabel("candidates per match")
    ax.legend(loc="upper right", handlelength=1.4)
    pf.save(fig, "false-positives", out)
    pf.plt.close(fig)


def machines_of(args):
    if args.run:
        return [tuple(spec.split("=", 1)) for spec in args.run]
    return list(pf.inputs("micro")["runs"].items())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", action="append", metavar="MACHINE=RUN",
                    help="machine label and run name; repeat per machine")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    loaded = []
    for machine, run in machines_of(args):
        df = load(run) if run else None
        if df is not None:
            whole = df[df["chunk_rows"] == ROW_GROUP]
            print(f"\n{machine} ({run})")
            print(df.groupby("dataset")[["num_rows", "payload_mb", "mean_len"]].first().round(1).to_string())
            print(whole.groupby(["dataset", "label"])["compression_ratio"].first().round(2).unstack().to_string())
            print(whole.groupby(["dataset", "label"])["gbps"].median().round(1).unstack().to_string())
        loaded.append((machine, df))

    if not any(df is not None for _, df in loaded):
        wanted = ", ".join(run or "-" for _, run in machines_of(args))
        raise SystemExit(f"no rows for {wanted}; run paper/gen-micro.sh first")

    lo, hi = drawn_values(loaded)
    GBS_LIM[:] = [lo * 0.8, hi * 1.3]
    print(f"GB/s axis fitted to the drawn medians: {GBS_LIM[0]:.2f} .. {GBS_LIM[1]:.0f}")

    print()
    dataset_panels(loaded, args.out)
    machine_panels(loaded, args.out)
    matrix_panel(loaded, args.out)
    fp_panel(loaded, args.out)


if __name__ == "__main__":
    main()
