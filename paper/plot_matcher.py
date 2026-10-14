# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2"]
# ///
"""Stage one: GB/s of every matcher against |C| cut codes and against |R| code
ranges, one column of panels per instruction set.

    uv run paper/plot_matcher.py [--data CSV ...] [--out DIR]

Each CSV is a `novel_mask_*.csv` from onpair's `scan::bench::matcher` or from
the FSST port's `prefilter_matcher_sweep`; see gen-kernel.sh and
gen-kernel-fsst.sh. The instruction set of a row is read off its
`machine` column.

Both encodings share a panel: onpair16's u16 codes solid, FSST's u8 codes
dashed, one colour per kernel. The background is banded by the u16 kernel
that measured fastest at each point of the sweep. The u16 build has no plain nibble bitmap, and
the u8 build no byte table, since there the bitmap takes every minimum cut the
table would. An encoding with no rows for an instruction set gets its fitted cost
model drawn faint in the same style, so a panel is laid out before the sweep
exists. GB/s is over the code stream, where a u8 code is one byte against
u16's two.
"""

import argparse
from pathlib import Path

import numpy as np
import pandas as pd

import paperfig as pf

HERE = Path(__file__).resolve().parent
ISAS = ["NEON", "AVX2", "AVX-512"]
FILE = {"NEON": "neon", "AVX2": "avx2", "AVX-512": "avx512"}
KERNELS = {"eq_or": "compare", "nibble_n_8k": r"nibble\_8k", "nibble": "bit-table", "range": "range",
           "table": "byte table"}
# The bit-table takes the byte table's colour: it is the same lookup over the
# whole code space, and the two never appear in one encoding, the byte table
# being u16's and the bit-table u8's.
KEY = {"eq_or": "eq_or", "nibble_n_8k": "nibble", "nibble": "table", "range": "range", "table": "table"}
BYTES_PER_CODE = {"onpair16": 2.0, "fsst": 1.0}
# The code stream every sweep walks, at which `bandwidth_line` reads the machine.
SWEEP_KIB = 1024
TICKS = [1, 2, 5, 10, 20, 50, 100]

# ns per code, mirroring policy::{neon, avx2, avx512bw}: (intercept, per cut
# code or per batch, per code range). Only read for an instruction set with no measurements.
# The u8 rows are onpair commit b081914's fit, which the FSST port carries.
MODEL = {
    "onpair16": {
        "NEON": {"eq_or": (0.0043, 0.0141, 0.0215), "nibble_n_8k": (0.0218, 0.0322, 0.0221),
                 "range": (0.0035, 0.0, 0.0213), "table": (0.189, 0.0, 0.0)},
        "AVX2": {"eq_or": (0.0136, 0.0105, 0.0239), "nibble_n_8k": (0.0594, 0.0310, 0.0225),
                 "range": (0.0084, 0.0, 0.0225), "table": (0.201, 0.0, 0.0)},
        "AVX-512": {"eq_or": (0.0079, 0.0113, 0.0127), "nibble_n_8k": (0.0270, 0.0141, 0.0119),
                    "range": (0.0085, 0.0, 0.0112), "table": (0.204, 0.0, 0.0)},
    },
    "fsst": {
        "NEON": {"eq_or": (0.0041, 0.0070, 0.0119), "nibble_n_8k": (0.0243, 0.0, 0.0115),
                 "nibble": (0.0386, 0.0, 0.0), "range": (0.0032, 0.0, 0.0111)},
        "AVX2": {"eq_or": (0.0124, 0.0052, 0.0104), "range": (0.0065, 0.0, 0.0104)},
        "AVX-512": {"eq_or": (0.0086, 0.0041, 0.0078), "nibble_n_8k": (0.0157, 0.0, 0.0075),
                    "range": (0.0050, 0.0, 0.0075)},
    },
}
MAX_BATCHES = {"onpair16": {"NEON": 3, "AVX2": 2, "AVX-512": 3}, "fsst": {"NEON": 1, "AVX2": 1, "AVX-512": 1}}

# The two encodings a panel carries, with the name a legend entry opens with
# and the line style that separates them.
ENCODINGS = [("onpair16", "u16", "-"), ("fsst", "u8", (0, (3, 1.2)))]
# The encoding whose fastest kernel bands the background.
BANDED = "onpair16"
# The u8 build has no byte table: its nibble bitmap takes every minimum cut
# the table would. The sweep still measures one, so drop it here rather than
# leave a series the build would never plan.
DROPPED = {"fsst": {"table"}}
# The width the |R| sweep is read at, the widest whose disjoint ranges span the
# grid out to |R| = 32: 16 codes in u16's 65536, 8 in u8's 256. A range costs
# two compares whatever it spans, so the two are the same measurement.
RANGE_WIDTH = {"onpair16": 16, "fsst": 8}

def isa_of(machine):
    m = machine.lower()
    if "avx512" in m:
        return "AVX-512"
    if "avx2" in m:
        return "AVX2"
    return "NEON"


def model_gbs(encoding, isa, kernel, k=0, r=0):
    a, slope, per_r = MODEL[encoding][isa][kernel]
    x = np.ceil(np.asarray(k) / 8) if kernel == "nibble_n_8k" else np.asarray(k)
    return BYTES_PER_CODE[encoding] / (a + slope * x + per_r * np.asarray(r))


def log_axis(ax, ticks):
    """Octave x axis with plain integer ticks."""
    ax.set_xscale("log", base=2)
    ax.set_xticks(ticks)
    ax.set_xticklabels([str(t) for t in ticks])
    ax.xaxis.set_minor_formatter(pf.mpl.ticker.NullFormatter())
    ax.set_xlim(ticks[0], ticks[-1] * 1.18)


def note_modelled(ax, tags):
    if tags:
        ax.text(0.5, 0.06, f"{', '.join(tags)}: cost model, not yet measured",
                transform=ax.transAxes, ha="center", fontsize=pf.SMALL_PT, color="#707070")


def shade_best(ax, series):
    """Band the background by the kernel with the highest rate at each swept
    x, `series` being one measured `pd.Series` per kernel over the same grid.
    Band edges sit at the geometric midpoints between neighbouring points."""
    table = pd.DataFrame(series).dropna(how="all")
    if table.empty:
        return
    best = table.idxmax(axis=1)
    x = best.index.to_numpy(dtype=float)
    mid = np.sqrt(x[1:] * x[:-1])
    edges = np.concatenate(([x[0] ** 2 / mid[0]], mid, [x[-1] ** 2 / mid[-1]]))
    start = 0
    for i in range(1, len(x) + 1):
        if i == len(x) or best.iloc[i] != best.iloc[start]:
            ax.axvspan(edges[start], edges[i], color=pf.COLOR[KEY[best.iloc[start]]],
                       alpha=0.13, linewidth=0, zorder=0)
            start = i


def blocks_of_8(g):
    """nibble_8k's cost moves only per block of 8 cut codes: one flat segment
    per measured block over (8b-8, 8b], joined by a straight line across
    blocks the sweep skipped."""
    v = g.groupby(np.ceil(g.index / 8).astype(int)).mean()
    x = np.column_stack([np.maximum(8 * v.index - 8, 1), 8 * v.index]).ravel()
    return x, np.repeat(v.values, 2)


def tokens_panel(ax, isa, measured):
    names = ["eq_or", "nibble_n_8k", "nibble", "table"]
    ks = np.arange(1, 257)
    # Kernel outer, encoding inner, so the legend lists the two encodings of a
    # kernel next to each other.
    swept, best = {}, {}
    for encoding, _tag, _style in ENCODINGS:
        rows = measured.get(encoding)
        swept[encoding] = None if rows is None else rows[(rows["ranges"] == 0) & (rows["count"] > 0)]
    for name in names:
        for encoding, tag, style in ENCODINGS:
            if name in DROPPED.get(encoding, ()):
                continue
            rows = swept[encoding]
            label, color = f"{tag}, {KERNELS[name]}", pf.COLOR[KEY[name]]
            if rows is not None:
                g = rows[rows["matcher"] == name].groupby("count")["gbs"].mean()
                if g.empty:
                    continue
                x, y = blocks_of_8(g) if name == "nibble_n_8k" else (g.index, g.values)
                ax.plot(x, y, linestyle=style, color=color, label=label)
                if encoding == BANDED:
                    best[name] = g
            elif name in MODEL[encoding][isa]:
                k = ks[ks <= 8 * MAX_BATCHES[encoding][isa]] if name == "nibble_n_8k" else ks
                ax.plot(k, model_gbs(encoding, isa, name, k=k), linestyle=style, color=color,
                        alpha=0.45, label=label)
    shade_best(ax, best)
    note_modelled(ax, [tag for e, tag, _s in ENCODINGS if measured.get(e) is None])
    log_axis(ax, [1, 4, 16, 64, 256])


def ranges_panel(ax, isa, measured):
    series = [("range", 0), ("nibble", 0), ("table", 1)]
    rs = np.arange(1, 33)
    swept, best = {}, {}
    for encoding, _tag, _style in ENCODINGS:
        rows = measured.get(encoding)
        width = RANGE_WIDTH[encoding]
        swept[encoding] = None if rows is None else rows[(rows["ranges"] > 0) & (rows["width"] == width)]
    for name, k in series:
        for encoding, tag, style in ENCODINGS:
            if name in DROPPED.get(encoding, ()):
                continue
            rows = swept[encoding]
            label, color = f"{tag}, {KERNELS[name]}", pf.COLOR[KEY[name]]
            if rows is not None:
                g = rows[(rows["matcher"] == name) & (rows["count"] == k)].groupby("ranges")["gbs"].mean()
                if g.empty:
                    continue
                x, y = blocks_of_8(g) if name == "nibble_n_8k" else (g.index, g.values)
                ax.plot(x, y, linestyle=style, color=color, label=label)
                if encoding == BANDED:
                    best[name] = g
            elif name in MODEL[encoding][isa]:
                ax.plot(rs, model_gbs(encoding, isa, name, k=k, r=rs), linestyle=style,
                        color=color, alpha=0.45, label=label)
    shade_best(ax, best)
    note_modelled(ax, [tag for e, tag, _s in ENCODINGS if measured.get(e) is None])
    log_axis(ax, [1, 2, 4, 8, 16, 32])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", nargs="+", default=[HERE / p for p in pf.inputs("kernel")["matcher"]])
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    figure = "kernel-matcher"

    df = pd.concat(pd.read_csv(path) for path in args.data)
    # onpair's 2026-09-10 scan refactor renamed the batched nibble kernel
    # `nibble_n_8k` -> `nibble_n8k`; older CSVs in data/ carry the old spelling.
    df = df.assign(matcher=df["matcher"].replace({"nibble_n8k": "nibble_n_8k"}))
    names = [encoding for encoding, _tag, _style in ENCODINGS]
    df = df[df["encoding"].isin(names) & df["matcher"].isin(KERNELS)]
    df["isa"] = df["machine"].map(isa_of)
    swept = {key: g for key, g in df.groupby(["isa", "encoding"])}
    measured = {isa: {enc: swept.get((isa, enc)) for enc in names} for isa in ISAS}

    # One y range over the panels.
    top = max(130.0, df["gbs"].max() * 1.15)
    ticks = [t for t in TICKS + [200, 500] if t <= top]

    # The legend pools the codes row before the ranges row: labelling the line
    # on the ranges row alone puts it last.
    def finish(ax, isa, xlabel, kind):
        ax.set_xlabel(xlabel)
        ax.set_ylabel("GB/s")
        if pf.inputs("kernel")["bandwidth_line"]:
            label = "read bandwidth, 1 MiB" if kind == "ranges" else None
            pf.bandwidth_line(ax, pf.read_bandwidth(isa, SWEEP_KIB), label)

    # One row of panels sweeps |C| over the three instruction sets, one sweeps
    # |R|, as `codes-<isa>` and `ranges-<isa>`, with one legend strip under
    # both rows pooled from a panel of each.
    legend_from = []
    for kind, draw, xlabel in (("codes", tokens_panel, r"$|C|$ cut codes"),
                               ("ranges", ranges_panel, r"$|R|$ code ranges")):
        panels = []
        for isa in ISAS:
            fig, ax = pf.panel(height=0.8 * 1.55)
            draw(ax, isa, measured[isa])
            finish(ax, isa, xlabel, kind)
            panels.append((fig, ax))
        pf.y_axis([ax for _, ax in panels], [ticks[0], top], ticks, "{:g}")
        legend_from.append(panels[0][1])
        pf.save_row(panels, figure, [f"{kind}-{FILE[isa]}" for isa in ISAS],
                    pf.PANEL_3UP, args.out)
    groups = [(KERNELS[k], [(f"{tag}, {KERNELS[k]}", tag) for _e, tag, _s in ENCODINGS])
              for k in ("eq_or", "nibble_n_8k", "range")]
    groups.insert(2, ("table", [("u16, byte table", "u16 byte"), ("u8, bit-table", "u8 bit")]))
    groups.append(("bandwidth", [("read bandwidth, 1 MiB", "read, 1 MiB")]))
    pf.save_grouped_legend(legend_from, groups, figure, args.out)


if __name__ == "__main__":
    main()
