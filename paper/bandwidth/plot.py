# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2"]
# ///
"""One core's read rate against working-set size, cold and warm.

    uv run paper/bandwidth/plot.py [--out DIR]

Reads every `results/*.csv` and draws two lines per machine: repeated passes
solid, the first pass over an evicted buffer dashed. Where they meet is the
last cache the stream fits in. Writes `kernel-bandwidth/sequential.pdf` beside
the other figures unless `--out` says otherwise. The panel carries its own
legend, since one line per machine fits inside the axes and a strip would cost
a figure's worth of height for four words.
"""

import argparse
import sys
from pathlib import Path

import pandas as pd

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import paperfig as pf  # noqa: E402

# The dev machine first, then one line per distinct core. `c8id.4xlarge` is
# measured and kept in `results/`, but it lies on the `c8id.xlarge` line to
# within a few percent, so drawing it only thickens that line.
ORDER = ["m4pro", "c8id.xlarge", "c7i.xlarge", "c7a.xlarge", "c8g.xlarge"]
LABEL = {"m4pro": "Apple M4 Pro", "c8id.xlarge": "c8id.xlarge (Granite Rapids)",
         "c7i.xlarge": "c7i.xlarge (Sapphire Rapids)",
         "c7a.xlarge": "c7a.xlarge (EPYC Genoa)",
         "c8g.xlarge": "c8g.xlarge (Graviton 4)"}
# One colour each, from the five distinct hues `paperfig` defines.
STYLE = {"m4pro": ("eq_or", "o"), "c8id.xlarge": ("nibble", "^"),
         "c7i.xlarge": ("bitmap", "v"), "c7a.xlarge": ("range", "D"),
         "c8g.xlarge": ("table", "s")}
# Ticks in KiB, labelled the way the sizes are spoken about.
TICKS = [64, 256, 1024, 4096, 16384, 65536, 262144, 1048576]
# The working set the matcher sweeps walk, 2^19 u16 codes or 2^20 u8 codes: a
# kernel measured there is bound by whatever each curve reads at this line.
SWEEP_KIB = 1024


def tick(kib):
    return f"{kib // 1024}M" if kib >= 1024 else f"{kib}K"


def panel(ax, data, ticks):
    ax.axvline(SWEEP_KIB, color="#707070", linestyle="--", linewidth=0.7, zorder=1)
    # Along the floor: both panels' curves climb away from it at this size.
    ax.text(SWEEP_KIB * 1.25, 0.03, "kernel sweep, 1 MiB", transform=ax.get_xaxis_transform(),
            fontsize=pf.SMALL_PT, color="#505050", ha="left", va="bottom")
    for machine in [m for m in ORDER if m in data]:
        d = data[machine].sort_values("kib")
        key, marker = STYLE[machine]
        ax.plot(d["kib"], d["seq_gbs"], marker=marker, markersize=2,
                color=pf.COLOR[key], label=LABEL[machine])
        if "cold_gbs" in d:
            ax.plot(d["kib"], d["cold_gbs"], linestyle="--", linewidth=0.8,
                    color=pf.COLOR[key])
    ax.set_xscale("log", base=2)
    ax.set_xticks(TICKS)
    ax.set_xticklabels([tick(t) for t in TICKS])
    ax.xaxis.set_minor_formatter(pf.mpl.ticker.NullFormatter())
    ax.set_yscale("log")
    ax.set_yticks(ticks)
    ax.set_yticklabels([str(t) for t in ticks])
    ax.set_xlabel("working set (bytes)")
    ax.set_ylabel("GB/s")
    ax.text(0.98, 0.04, "dashed: first pass, buffer evicted", transform=ax.transAxes,
            fontsize=pf.SMALL_PT, color="#505050", ha="right", va="bottom")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    files = sorted((HERE / "results").glob("*.csv"))
    if not files:
        raise SystemExit("no results/*.csv — run run.sh first")
    df = pd.concat((pd.read_csv(f) for f in files), ignore_index=True)
    data = {m: g for m, g in df.groupby("machine")}

    fig, ax = pf.panel(pf.COLUMN_IN, height=1.9)
    panel(ax, data, [10, 20, 50, 100, 200])
    ax.legend(loc="lower left", handlelength=1.4, fontsize=pf.SMALL_PT,
              labelspacing=0.25, borderpad=0.3)
    pf.save_panel(fig, "kernel-bandwidth", "sequential", args.out)


if __name__ == "__main__":
    main()
