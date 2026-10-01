# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "duckdb>=1.1", "numpy"]
# ///
"""Micro-benchmark figures from `paper/specs/micro-1mi*.toml` runs: our prefilter
at 12 and 16 dictionary bits against decompress-then-memmem, plaintext memmem
and the FSST-LIKE interpreter, on every machine given.

    uv run paper/plot_micro.py [--run "ARM, NEON=<run>" --run ...] [--jit-run RUN] [--out DIR]

The runs default to the [micro] and [breakdown] sections of paper/inputs.toml.

One PDF per panel, the grid assembled in LaTeX with subfigure. A panel is one
(dataset, machine) pair, numbered across then down, beside a legend strip:
`<dimension>_1.pdf` ... `<dimension>_6.pdf` and `<dimension>_legend.pdf`. The
dimensions are:

    micro-selectivity  GB/s against row selectivity, at a 512k-row row group
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

import paperfig as pf

HERE = Path(__file__).resolve().parent
DB = HERE.parent / "results" / "bench.duckdb"

# Panel titles carry both the column and the machine, so both names are short
# enough for a third of the text width. Every column the micro specs measure, in
# panel order.
TITLES = {"imdb-mcnote-1mi": "Company notes", "amazon-title-1mi": "Book titles",
          "clickbench-url-1mi": "URL", "dbpedia-abstract-1mi": "DBpedia abstracts"}
DATASET_ORDER = list(TITLES)
# The instruction sets `micro-arch` draws.
ARCH_MACHINES = ["ARM, NEON", "x86, AVX-512"]
# Each series is the compression scheme it stores and the way it searches:
# "pattern automaton" is Pop et al.'s compression-aware LIKE over FSST.
# Legend order, which is also the draw order: the legend is laid out in four
# columns of two, and matplotlib fills a column at a time, so the pairs here
# are the pairs on the page. The JIT backend is not in this list; it appears
# only in `micro-breakdown`, where its compile is the point.
SYSTEMS = [
    ("onpair_spiral/pf_walk@bits=12,prune=1.0", "OnPair12, minimum cut scan", "#56B4E9", "s"),
    ("onpair_spiral/pf_walk@bits=16,prune=1.0", "OnPair16, minimum cut scan", "#0072B2", "o"),
    ("fsst_like_tum/interp", "FSST, pattern automaton", "#009E73", "D"),
    # The same planner and walk over an FSST symbol table.
    ("fsst_prefilter/fsst-prefilter", "FSST, minimum cut scan", "#CC79A7", "P"),
    ("fsst/decode+memmem-hay", "FSST, decompress + memmem", "#E69F00", "v"),
    ("onpair/decode+memmem-hay", "OnPair, decompress + memmem", "#D55E00", "^"),
    ("uncompressed/direct+memmem-hay", "Uncompressed, memmem", "#666666", "x"),
]
LEGEND_COLS = 4
# The two decode baselines are drawn as one flat dashed line at their median
# over the panel's x, without markers or a quartile band: they are the level
# the compressed-domain series is read against, not a shape worth a curve.
# Uncompressed memmem is a curve, since its rate moves with selectivity.
BASELINES = {
    "onpair/decode+memmem-hay",
    "fsst/decode+memmem-hay",
}


def series(ax, label, name, color, marker, x, y):
    if label in BASELINES:
        ax.axhline(np.median(y), color=color, linestyle="--", linewidth=1.0, label=name)
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
        select dataset, label, chunk_rows, needle_len, selectivity, gbps, compression_ratio,
               num_rows, payload_mb, mean_len,
               total_ns, setup_ns, decode_ns, scan_ns, prefilter_ns, verify_ns
        from v where run_name = ? and is_latest and status = 'ok'
        """,
        params=[run],
    ).df()


# The throughput range every GB/s panel shares, so the panels compare; main()
# replaces it with the range the loaded runs actually cover.
GBS_LIM = [0.1, 300.0]


def gbs_axis(ax):
    lo, hi = GBS_LIM
    ax.set_yscale("log")
    ax.set_ylim(lo, hi)
    ticks = [t for t in (0.01, 0.1, 1, 10, 100, 1000) if lo <= t <= hi]
    ax.set_yticks(ticks)
    ax.set_yticklabels([("%g" % t) for t in ticks])


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


def selectivity_panel(ax, d):
    """Every query as a point, the median of each half-decade bin as the line."""
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
    med, lo, hi = pooled(df, x)
    for label, name, color, marker in systems:
        if label not in med.index.get_level_values(0):
            continue
        m = med.loc[label]
        if band and label not in BASELINES:
            ax.fill_between(m.index, lo.loc[label].values, hi.loc[label].values,
                            color=color, alpha=0.15, linewidth=0)
        series(ax, label, name, color, marker, m.index, m.values)
    gbs_axis(ax)


# The needle length buckets the matrix draws: a column per power of two, with
# 1 and 2 bytes apart from 3-4. Needles past 64 bytes are left out.
MATRIX_LEN_EDGES = [1, 2, 4, 8, 16, 32, 64]
MATRIX_LEN_LABELS = ["1", "2", "3-4", "5-8", "9-16", "17-32", "33-64"]


def selectivity_x(df):
    d = df.copy()
    live = d["selectivity"] > 0
    d["sel"] = 0.0
    d.loc[live, "sel"] = 10 ** ((np.log10(d.loc[live, "selectivity"]) * 2).round() / 2)
    return d


def chunk_x(df):
    """Row group size, with the whole column at the nominal 1m: the three
    columns differ there (901k rows for IMDb names) and one tick has to serve."""
    return df.assign(chunk=df["chunk_rows"].where(df["chunk_rows"] > 0, 1_000_000))


def drawn_values(loaded):
    """Every y the GB/s panels actually draw: the row group medians and their
    quartile bands, and the per-column and per-machine medians. The axis is
    fitted to these rather than to single queries, which run far below any
    median and would otherwise leave the panels mostly empty."""
    df = pd.concat([d for _, d in loaded if d is not None], ignore_index=True)
    at = df[df["chunk_rows"] == ROW_GROUP]
    vals = []
    vals.extend(pooled(chunk_x(df), "chunk"))
    for dataset, _title in datasets_of(loaded):
        vals.append(selectivity_x(at[at["dataset"] == dataset])
                    .groupby(["label", "sel"])["gbps"].median())
    for _machine, d in loaded:
        if d is not None:
            vals.append(selectivity_x(d[d["chunk_rows"] == ROW_GROUP])
                        .groupby(["label", "sel"])["gbps"].median())
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
    """A panel is drawn at the width it prints at; four to a row is 0.23 of
    the text width, three 0.32, two 0.49."""
    return {2: pf.PANEL_2UP, 4: pf.PANEL_4UP}.get(n, pf.PANEL_3UP)


# One panel per system, drawn on one colour scale so the cells compare across
# panels. The minimum cut scan is the subject; decompress-then-memmem is the
# control, since the same cell there is the same needle on the same rows with
# the length effect removed.
# Slowest average cell first, as the paper orders them: the first panel
# carries the row labels. FSST's minimum cut scan is drawn but not in the paper.
MATRIX_SYSTEMS = [("fsst_like_tum/interp", "fsst-automaton"),
                  ("onpair/decode+memmem-hay", "onpair-decode"),
                  ("uncompressed/direct+memmem-hay", "uncompressed"),
                  ("onpair_spiral/pf_walk@bits=12,prune=1.0", "onpair12-cut"),
                  ("fsst_prefilter/fsst-prefilter", "fsst-cut")]
# Rows are decades of selectivity under a zero-match row; columns are the
# needle panel's buckets.
MATRIX_SEL_EDGES = [1e-5, 1e-4, 1e-3, 1e-2, 1e-1]
MATRIX_SEL_LABELS = ["0 rows", r"$<$0.001\%", r"0.001--0.01\%", r"0.01--0.1\%",
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
    """Mean GB/s per (selectivity bucket, needle length bucket) for one
    system."""
    d = df[df["label"] == label]
    return None if d.empty else matrix_pivot(d, "gbps", "mean")


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


def matrix_legend(norm, cmap, out):
    """The shared colour scale as `micro-matrix/legend.pdf`, cropped to the bar
    and its labels so LaTeX includes it at its natural size."""
    pf.style()
    fig = pf.plt.figure(figsize=(pf.TEXT_IN, 0.4))
    cax = fig.add_axes([0.3, 0.55, 0.36, 0.18])
    bar = fig.colorbar(pf.mpl.cm.ScalarMappable(norm=norm, cmap=cmap), cax=cax,
                       orientation="horizontal")
    matrix_colorbar(bar, norm, bar.ax.xaxis)
    cax.text(1.02, 0.5, "GB/s", transform=cax.transAxes, va="center",
             fontsize=pf.SMALL_PT)
    pf.save(fig, "micro-matrix/legend", out, bbox_inches="tight")
    pf.plt.close(fig)


def matrix_panel(loaded, out):
    """`micro-matrix/<system>`: mean GB/s per (selectivity, needle length)
    cell on ClickBench URLs, one panel per system on a shared colour scale, which is
    `micro-matrix/legend`. The trend panels pool one axis over the other,
    which cannot separate "long needles are rare" from "long needles scan
    faster"; reading down a column of this one holds needle length fixed."""
    pf.clear_figure("micro-matrix", out)
    df = pd.concat([d for _, d in loaded if d is not None], ignore_index=True)
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
    pf.save_row(panels, "micro-matrix", [name for name, _ in grids], pf.PANEL_4UP, out)
    matrix_legend(norm, MATRIX_CMAP, out)


def trend_panels(loaded, out):
    """`micro-trends/rowgroup`: throughput against row group size over every
    column and machine at once, one column wide, its legend above it."""
    pf.clear_figure("micro-trends", out)
    df = pd.concat([d for _, d in loaded if d is not None], ignore_index=True)
    fig, ax = pf.panel(width=pf.COLUMN_IN)
    d = chunk_x(df)
    trend_panel(ax, d, "chunk")
    chunks = sorted(d["chunk"].unique())
    ax.set_xscale("log", base=2)
    ax.set_xticks(chunks)
    ax.set_xticklabels([chunk_label(c) for c in chunks])
    ax.xaxis.set_minor_formatter(pf.mpl.ticker.NullFormatter())
    ax.set_xlabel("row group size")
    ax.set_ylabel("uncompressed GB/s")
    pf.save_legend(ax, "micro-trends", 2, out, width=pf.COLUMN_IN)
    pf.save_panel(fig, "micro-trends", "rowgroup", out)


def facet_panel(ax, df):
    """Every system against selectivity, medians per half-decade bin."""
    d = selectivity_x(df)
    for label, name, color, marker in SYSTEMS:
        m = d[d["label"] == label].groupby("sel")["gbps"].median()
        if m.empty:
            continue
        series(ax, label, name, color, marker, m.index, m.values)
    pf.selectivity_axis(ax)
    gbs_axis(ax)
    ax.set_ylabel("uncompressed GB/s")


def dataset_panels(loaded, out):
    """`micro-datasets/1..3`: one column per panel, every system, pooled over
    the machines."""
    pf.clear_figure("micro-datasets", out)
    df = pd.concat([d for _, d in loaded if d is not None], ignore_index=True)
    df = df[df["chunk_rows"] == ROW_GROUP]
    drawn = datasets_of(loaded)
    width = dataset_width(len(drawn))
    panels = []
    for dataset, _title in drawn:
        fig, ax = pf.panel(width=width)
        facet_panel(ax, df[df["dataset"] == dataset])
        panels.append((fig, ax))
    pf.save_row(panels, "micro-datasets", [pf.slug(t) for _, t in drawn], width, out)
    pf.save_legend(panels[-1][1], "micro-datasets", LEGEND_COLS, out)


def machine_panels(loaded, out):
    """`micro-arch/<set>`: one instruction set per panel, every system, pooled
    over the columns, two panels in one column of the page."""
    pf.clear_figure("micro-arch", out)
    width = 0.48 * pf.COLUMN_IN
    panels, stems = [], []
    for machine, df in loaded:
        if df is None or machine not in ARCH_MACHINES:
            continue
        fig, ax = pf.panel(width=width)
        facet_panel(ax, df[df["chunk_rows"] == ROW_GROUP])
        panels.append((fig, ax))
        stems.append(pf.slug(machine))
    if panels:
        pf.save_row(panels, "micro-arch", stems, width, out, unit=pf.COLUMN_IN)
        pf.save_legend(panels[-1][1], "micro-arch", 2, out, width=pf.COLUMN_IN)


# --------------------------------------------------------------- breakdown
# Where a query's time goes, as a stacked horizontal bar per system at one row
# group size. This is the one figure that draws the JIT backend, because its
# compile is charged per chunk and only a breakdown shows that.
# Three families, each a heading with its members under it: the heading names
# how the system searches, the member what it searches over, so no label has to
# carry both. Members are ordered fastest first at draw time.
BREAKDOWN = [
    ("minimum cut", [
        ("onpair_spiral/pf_walk@bits=16,prune=1.0", "OnPair16"),
        ("onpair_spiral/pf_walk@bits=12,prune=1.0", "OnPair12"),
        ("fsst_prefilter/fsst-prefilter", "FSST"),
    ]),
    ("automaton", [
        ("fsst_like_tum/interp", "interpreted"),
        ("fsst_like_tum/llvm-simd", "LLVM JIT"),
    ]),
    ("memmem", [
        ("uncompressed/direct+memmem-hay", "uncompressed"),
        ("onpair/decode+memmem-hay", "OnPair"),
        ("fsst/decode+memmem-hay", "FSST"),
    ]),
]
# In the order they happen: prep is a symbol table build, an automaton build or
# the JIT's compile; decompress is a decode-first baseline materialising the
# plaintext; verify is the exact check the minimum cut scans run behind their
# probe scan; match is everything left, so the four sum to the measured total
# rather than to a sum of separately taken medians.
#
# Only the two minimum cut systems have a verify phase to name, and both now
# report it: `verify_ns` is the alignment walks alone, over the hits the
# kernels produced, timed as its own pass. That pass re-walks rows the fused
# scan had in registers, so the segment is an upper bound on what the walk
# costs inside the scan, and `match` beside it is the matching lower bound.
# The other systems report no verify phase and get no segment, which is their
# architecture rather than their instrumentation.
SEGMENTS = [("prep", "#E69F00"), ("decompress", "#009E73"),
            ("verify", "#CC79A7"), ("match", "#0072B2")]
BREAKDOWN_CHUNK = 524288
# One instruction set rather than a pool: the phases are a cost breakdown, not
# a portability claim, and the JIT backend registers on x86 alone.
BREAKDOWN_MACHINE = "x86, AVX-512"


def breakdown(df):
    """ms per million rows per system, split into the three segments. Each
    column contributes its own median query and the three are averaged. Per
    million rows rather than per row because a row costs single digit to tens
    of nanoseconds; the two differ only in the name, one ns per row being one
    ms per million rows."""
    d = df[df["chunk_rows"] == BREAKDOWN_CHUNK].copy()
    for c in ("setup_ns", "decode_ns", "verify_ns"):
        d[c] = d[c].fillna(0)
    g = d.groupby(["label", "dataset"]).median(numeric_only=True).reset_index()
    unit = g["num_rows"] / 1e6
    out = pd.DataFrame({
        "label": g["label"],
        "prep": g["setup_ns"] / 1e6 / unit,
        "decompress": g["decode_ns"] / 1e6 / unit,
        "verify": g["verify_ns"] / 1e6 / unit,
        "total": g["total_ns"] / 1e6 / unit,
    })
    # The verify pass is timed on its own and is slower than the fused walk it
    # measures, so on a system where it is most of the scan it can eat the
    # whole remainder. Cap it at what is left rather than let it push `match`
    # negative and the bar past its own total.
    room = (out["total"] - out["prep"] - out["decompress"]).clip(lower=0)
    out["verify"] = out[["verify"]].join(room.rename("room")).min(axis=1)
    out["match"] = room - out["verify"]
    return out.groupby("label").mean(numeric_only=True)


def breakdown_panel(loaded, jit, out):
    """`micro-breakdown/<row group>`: one bar per system under its family's
    heading, one column wide, its legend inside. One instruction set only, see
    BREAKDOWN_MACHINE, plus the JIT backend's bar from `jit`, its own run."""
    pf.clear_figure("micro-breakdown", out)
    frames = [d for m, d in loaded if d is not None and m == BREAKDOWN_MACHINE]
    if not frames:
        print(f"micro-breakdown: no run for {BREAKDOWN_MACHINE}, skipped")
        return
    df = pd.concat(frames + [jit], ignore_index=True)
    b = breakdown(df)
    fig, ax = pf.panel(width=pf.COLUMN_IN, height=2.3)
    # A heading row carries no bar; its members are indented under it and
    # sorted by what they cost. Rows inside a family sit a step apart, with
    # extra space above each heading so the families read as blocks.
    STEP, FAMILY_GAP = 0.82, 0.5
    bars, ticks, labels, y = [], [], [], 0.0
    for heading, members in BREAKDOWN:
        members = sorted((m for m in members if m[0] in b.index),
                         key=lambda m: b.loc[m[0], "total"])
        if not members:
            continue
        if bars:
            y += FAMILY_GAP
        ticks.append(-y)
        labels.append(r"\textbf{%s}" % heading)
        y += STEP
        for lab, name in members:
            bars.append((lab, -y))
            ticks.append(-y)
            labels.append(r"\hspace*{0.7em}%s" % name)
            y += STEP
    for lab, yy in bars:
        left = 0.0
        for seg, color in SEGMENTS:
            w = b.loc[lab, seg]
            if w <= 0:
                continue
            ax.barh(yy, w, left=left, height=0.62, color=color, linewidth=0)
            left += w
    ax.set_xlim(0, max(b.loc[lab, "total"] for lab, _ in bars) * 1.04)
    ax.set_yticks(ticks)
    ax.set_yticklabels(labels)
    ax.tick_params(axis="y", length=0)
    ax.set_ylim(-y + STEP - 0.55, 0.45)
    ax.set_xlabel("ms per million rows")
    ax.grid(axis="x", alpha=0.3)
    ax.set_axisbelow(True)
    # The short bars leave the top right empty, which is where the key goes:
    # one figure, one file. Built in SEGMENTS order rather than draw order.
    ax.legend(handles=[pf.mpl.patches.Patch(color=c, label=n) for n, c in SEGMENTS],
              loc="upper right", handlelength=1.2, handletextpad=0.5,
              borderaxespad=0.2, labelspacing=0.3)
    pf.save_panel(fig, "micro-breakdown", chunk_label(BREAKDOWN_CHUNK), out)


def machines_of(args):
    if args.run:
        return [tuple(spec.split("=", 1)) for spec in args.run]
    return list(pf.inputs("micro").items())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", action="append", metavar="MACHINE=RUN",
                    help="machine label and run name; repeat per machine")
    ap.add_argument("--jit-run", default=pf.inputs("breakdown")["jit"],
                    help="the JIT backend's run, drawn in micro-breakdown")
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
        raise SystemExit(f"no rows for {wanted}; run paper/gen-micro-data.sh first")

    lo, hi = drawn_values(loaded)
    GBS_LIM[:] = [lo * 0.8, hi * 1.3]
    print(f"GB/s axis fitted to the drawn medians: {GBS_LIM[0]:.2f} .. {GBS_LIM[1]:.0f}")

    print()
    trend_panels(loaded, args.out)
    dataset_panels(loaded, args.out)
    machine_panels(loaded, args.out)
    matrix_panel(loaded, args.out)
    breakdown_panel(loaded, load(args.jit_run), args.out)


if __name__ == "__main__":
    main()
