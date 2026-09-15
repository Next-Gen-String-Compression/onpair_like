# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2"]
# ///
"""Stage one: GB/s of every matcher against K tokens and against R ranges,
one column of panels per instruction set.

    uv run paper/plot_matcher.py [--data CSV ...] [--out DIR] [--encoding onpair16|fsst]

Each CSV is a `novel_mask_*.csv` from onpair's `scan::bench::matcher` or from
the FSST port's `prefilter_matcher_sweep`; see gen-kernel-data.sh and
gen-kernel-data-fsst.sh. The instruction set of a row is read off its
`machine` column. An instruction set with no rows gets the fitted cost model
drawn dashed as a placeholder, so the panel is laid out before the sweep
exists. One encoding is drawn per run: onpair16 (u16 codes, two bytes each)
into kernel-matcher/, fsst (u8 codes) into kernel-matcher-fsst/, which has the
universal nibble bitmap the u16 build dropped and no byte table, since the
bitmap takes every cover the table would.
"""

import argparse
from pathlib import Path

import numpy as np
import pandas as pd

import paperfig as pf

HERE = Path(__file__).resolve().parent
ISAS = ["NEON", "AVX2", "AVX-512"]
FILE = {"NEON": "neon", "AVX2": "avx2", "AVX-512": "avx512"}
KERNELS = {"eq_or": "compare", "nibble_n_8k": "nibble table", "nibble": "bitmap", "range": "range",
           "table": "byte table"}
KEY = {"eq_or": "eq_or", "nibble_n_8k": "nibble", "nibble": "bitmap", "range": "range", "table": "table"}
BYTES_PER_CODE = {"onpair16": 2.0, "fsst": 1.0}
TICKS = [1, 2, 5, 10, 20, 50, 100]

# ns per code, mirroring policy::{neon, avx2, avx512bw}: (intercept, per K or
# per batch, per R). Only read for an instruction set with no measurements.
# The u8 rows are onpair commit b081914's fit, which the FSST port carries.
MODEL = {
    "onpair16": {
        "NEON": {"eq_or": (0.0043, 0.0144, 0.0205), "nibble_n_8k": (0.0228, 0.0325, 0.0213),
                 "range": (0.0043, 0.0, 0.0212), "table": (0.195, 0.0, 0.0)},
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
ENCODING = "onpair16"


def isa_of(machine):
    m = machine.lower()
    if "avx512" in m:
        return "AVX-512"
    if "avx2" in m:
        return "AVX2"
    return "NEON"


def model_gbs(isa, kernel, k=0, r=0):
    a, slope, per_r = MODEL[ENCODING][isa][kernel]
    x = np.ceil(np.asarray(k) / 8) if kernel == "nibble_n_8k" else np.asarray(k)
    return BYTES_PER_CODE[ENCODING] / (a + slope * x + per_r * np.asarray(r))


def log_axis(ax, ticks):
    """Octave x axis with plain integer ticks."""
    ax.set_xscale("log", base=2)
    ax.set_xticks(ticks)
    ax.set_xticklabels([str(t) for t in ticks])
    ax.xaxis.set_minor_formatter(pf.mpl.ticker.NullFormatter())
    ax.set_xlim(ticks[0] * 0.85, ticks[-1] * 1.18)


def tokens_panel(ax, isa, rows):
    names = ["eq_or", "nibble_n_8k", "nibble", "table"]
    if rows is not None:
        sub = rows[(rows["ranges"] == 0) & (rows["count"] > 0)]
        for name in names:
            g = sub[sub["matcher"] == name].groupby("count")["gbs"].mean()
            if g.empty:
                continue
            ax.plot(g.index, g.values, marker=pf.MARKER[KEY[name]], color=pf.COLOR[KEY[name]], markersize=2, label=KERNELS[name])
    else:
        ks = np.arange(1, 257)
        for name in names:
            if name not in MODEL[ENCODING][isa]:
                continue
            k = ks[ks <= 8 * MAX_BATCHES[ENCODING][isa]] if name == "nibble_n_8k" else ks
            ax.plot(k, model_gbs(isa, name, k=k), "--", color=pf.COLOR[KEY[name]], alpha=0.6, label=KERNELS[name])
        ax.text(0.5, 0.06, "cost model, not yet measured", transform=ax.transAxes,
                ha="center", fontsize=pf.SMALL_PT, color="#707070")
    log_axis(ax, [1, 4, 16, 64, 256])


def ranges_panel(ax, isa, rows):
    series = [("range", 0, "range"), ("nibble", 0, "bitmap"), ("table", 1, "byte table")]
    if rows is not None:
        sub = rows[(rows["ranges"] > 0) & (rows["width"] == 16)]
        for name, k, label in series:
            g = sub[(sub["matcher"] == name) & (sub["count"] == k)].groupby("ranges")["gbs"].mean()
            if g.empty:
                continue
            ax.plot(g.index, g.values, marker=pf.MARKER[KEY[name]], color=pf.COLOR[KEY[name]], markersize=2, label=label)
    else:
        rs = np.arange(1, 33)
        for name, k, label in series:
            if name not in MODEL[ENCODING][isa]:
                continue
            ax.plot(rs, model_gbs(isa, name, k=k, r=rs), "--", color=pf.COLOR[KEY[name]], alpha=0.6, label=label)
        ax.text(0.5, 0.06, "cost model, not yet measured", transform=ax.transAxes,
                ha="center", fontsize=pf.SMALL_PT, color="#707070")
    log_axis(ax, [1, 2, 4, 8, 16, 32])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", nargs="+", default=sorted(HERE.glob("data/matcher_*.csv")))
    ap.add_argument("--out", default=None)
    ap.add_argument("--encoding", default="onpair16", choices=sorted(MODEL))
    args = ap.parse_args()
    global ENCODING
    ENCODING = args.encoding
    figure = "kernel-matcher" if ENCODING == "onpair16" else f"kernel-matcher-{ENCODING}"

    df = pd.concat(pd.read_csv(path) for path in args.data)
    # onpair's 2026-09-10 scan refactor renamed the batched nibble kernel
    # `nibble_n_8k` -> `nibble_n8k`; older CSVs in data/ carry the old spelling.
    df = df.assign(matcher=df["matcher"].replace({"nibble_n8k": "nibble_n_8k"}))
    df = df[(df["encoding"] == ENCODING) & df["matcher"].isin(KERNELS)]
    df["isa"] = df["machine"].map(isa_of)
    measured = {isa: g for isa, g in df.groupby("isa")}

    def finish(ax, xlabel):
        ax.set_yscale("log")
        ax.set_yticks(TICKS)
        ax.set_yticklabels([str(t) for t in TICKS])
        ax.set_ylim(0.9, 130)
        ax.set_xlabel(xlabel)
        ax.set_ylabel("GB/s")

    # One row of panels sweeps K over the three instruction sets, one sweeps
    # R, as `tokens-<isa>` and `ranges-<isa>`, with one legend strip under
    # both rows pooled from a panel of each.
    legend_from = []
    for col, isa in enumerate(ISAS):
        fig, ax = pf.panel()
        tokens_panel(ax, isa, measured.get(isa))
        finish(ax, "K tokens")
        if col == 0:
            legend_from.append(ax)
        pf.save_panel(fig, figure, f"tokens-{FILE[isa]}", args.out)
    for col, isa in enumerate(ISAS):
        fig, ax = pf.panel()
        ranges_panel(ax, isa, measured.get(isa))
        finish(ax, "R ranges")
        if col == 0:
            legend_from.append(ax)
        pf.save_panel(fig, figure, f"ranges-{FILE[isa]}", args.out)
    pf.save_legend(legend_from, figure, 5 if ENCODING == "fsst" else 4, args.out)


if __name__ == "__main__":
    main()
