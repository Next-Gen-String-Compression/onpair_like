# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "duckdb>=1.1", "numpy", "pyarrow>=17", "xxhash>=3"]
# ///
"""Figure 1: GB/s over the selected rows, one panel per pre-selection, one
horizontal bar per system, the median over the needles of ClickBench URLs with
its value at the bar end, on NEON. Two groups: searching the raw strings,
stored uncompressed or decompressed first, and searching the compressed codes.
LZ4, uncompressed, OnPair16 and the FSST SIMD-cut come from specs/fig1/, FSST
decompress and the automaton from the NEON selection run.

    uv run paper/plot_fig1.py [--selection RUN] [--lz4 RUN] [--uncompressed RUN] [--onpair RUN] [--fsst RUN] [--out DIR]

Writes `fig1.pdf`, one column wide, the rows labelled on the left of the first panel.
"""

import argparse

import paperfig as pf
from config import COLOR, METHOD, SCHEME
import plot_micro as pm
import plot_selection as ps

def cap(text):
    return text[0].upper() + text[1:]


# (group, [(label, row label, colour)]) top to bottom, method before scheme.
GROUPS = [
    ("Search raw strings", [
        ("uncompressed_memmem/row-hay", SCHEME["uncompressed"], COLOR["uncompressed"]),
        ("lz4/decode+memmem-hay", f"Decompress, {SCHEME['lz4']}", COLOR["lz4-decode"]),
        ("fsst/decode+memmem-hay", f"Decompress, {SCHEME['fsst']}", COLOR["fsst-decode"]),
    ]),
    ("Search compressed", [
        ("fsst_like_tum/interp", f"{cap(METHOD['automaton'])}, {SCHEME['fsst']}", COLOR["fsst-automaton"]),
        ("simd_cut_fsst/simd-cut", f"{cap(METHOD['cut'])}, {SCHEME['fsst']}", COLOR["fsst-cut"]),
        ("simd_cut_onpair/simd-cut@bits=16,prune=1.0", f"{cap(METHOD['cut'])}, {SCHEME['onpair']}", COLOR["onpair16-cut"]),
    ]),
]
SYSTEMS = [label for _, members in GROUPS for label, _, _ in members]
# The system whose values go inside its bars, in white; every other value
# follows its bar, in black.
INSIDE = "simd_cut_onpair/simd-cut@bits=16,prune=1.0"
# The one column Figure 1 draws; Section 6 has all four.
DATASET = "clickbench-url-1mi"
# Each panel's GB/s axis as segments (from, to, ticks), each as wide as it is
# long, so a GB/s is the same length everywhere. The no-filter panel leaves out
# 50-115, where no bar ends; a bar past 50 goes on in the second segment.
PANELS = (
    [(0, 30, (0, 20))],
    [(0, 30, (0, 20))],
    [(0, 50, (0, 20, 40)), (115, 140, (120,))],
)
# Labelled 1% and 10% until a run at those shares replaces 2^-7 and 2^-3.
SHARES = [(2**-7, r"1\% selected"), (2**-3, r"10\% selected"), (1.0, "full scan")]


# Bar height, of one row.
BAR = 0.85
# Points the systems are indented under their method.
INDENT = 7


def draw(d):
    """A group header row, then one bar per system, top to bottom."""
    rows, y = [], 0
    for group, members in GROUPS:
        rows.append((y, group, None, None))
        y += 1
        for label, text, color in members:
            rows.append((y, text, label, color))
            y += 1
    pf.style()
    segments = [segment for panel in PANELS for segment in panel]
    fig, axes = pf.plt.subplots(1, len(segments), figsize=(pf.COLUMN_IN, 1.75), sharey=True,
                                gridspec_kw={"width_ratios": [to - lo for lo, to, _ in segments]},
                                constrained_layout=True)
    remaining = iter(axes)
    for (share, title), panel in zip(SHARES, PANELS):
        at = d[d["share"] == share].groupby("label")["gbps"].median()
        panel_axes = [next(remaining) for _ in panel]
        for k, (ax, (lo, to, ticks)) in enumerate(zip(panel_axes, panel)):
            later = panel[k + 1][0] if k + 1 < len(panel) else float("inf")
            for yy, _, label, color in rows:
                if label is None or label not in at or (k > 0 and at[label] <= lo):
                    continue
                m = at[label]
                ax.barh(yy, min(m, to) - lo, BAR, left=lo, color=color)
                # The value where the bar ends: here, unless it goes on in the next segment.
                if m < later:
                    inside = label == INSIDE
                    ax.annotate(f"{m:.1f}" if m >= 0.1 else "$<$0.1", (min(m, to), yy),
                                xytext=(-1.5 if inside else 1.5, 0), textcoords="offset points",
                                ha="right" if inside else "left", va="center",
                                color="white" if inside else "black", fontsize=pf.SMALL_PT, zorder=4,
                                annotation_clip=False)
            ax.set_xlim(lo, to)
            # No tick at a segment's end, where its label would meet the next one's.
            ax.set_xticks(ticks)
            ax.grid(axis="y", visible=False)
            if k > 0:
                ax.spines["left"].set_visible(False)
                ax.tick_params(axis="y", left=False)
            if k + 1 < len(panel):
                # The axis is broken between the two segments.
                for side, at_x in ((ax, 1), (panel_axes[k + 1], 0)):
                    side.plot([at_x], [0], transform=side.transAxes, marker=[(-1, -1), (1, 1)], markersize=5,
                              linestyle="none", color="black", mew=0.8, clip_on=False)
        # The share in the panel's empty top row, the group header's.
        panel_axes[0].annotate(title, (0, rows[0][0]), xycoords=("axes fraction", "data"), xytext=(2, 0),
                               textcoords="offset points", ha="left", va="center", fontsize=pf.SMALL_PT - 1.5,
                               bbox=dict(facecolor="white", edgecolor="none", pad=0.5))
    # The unit once, in the tick labels' row, ending with the last axis.
    axes[-1].annotate("GB/s", (1, 0), xycoords="axes fraction", xytext=(0, -pf.mpl.rcParams["xtick.major.pad"]
                      - pf.mpl.rcParams["xtick.major.size"]), textcoords="offset points", ha="right", va="top",
                      fontsize=pf.SMALL_PT)
    axes[0].set_yticks([])
    axes[0].set_ylim(rows[-1][0] + 0.6, -0.6)
    # Left-aligned row labels, the systems indented under their group.
    texts = [(axes[0].annotate(rf"\textbf{{{t}}}" if label is None else t,
                               (0, yy), xycoords=("axes fraction", "data"), xytext=(0, 0), textcoords="offset points",
                               ha="left", va="center", fontsize=pf.SMALL_PT),
              label is not None)
             for yy, t, label, _ in rows]
    fig.canvas.draw()
    width = max(t.get_window_extent().width + (INDENT if sub else 0) for t, sub in texts) * 72 / fig.dpi + 3
    for t, sub in texts:
        t.set_position((-width + (INDENT if sub else 0), 0))
    # Measured where they landed: move them clear of the axis.
    fig.canvas.draw()
    over = (max(t.get_window_extent().x1 for t, _ in texts) - axes[0].bbox.x0) * 72 / fig.dpi + 3
    if over > 0:
        for t, sub in texts:
            t.set_position((-width - over + (INDENT if sub else 0), 0))
    return fig


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--selection", default=None)
    ap.add_argument("--lz4", default=None)
    ap.add_argument("--uncompressed", default=None)
    ap.add_argument("--onpair", default=None)
    ap.add_argument("--fsst", default=None)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    runs = pf.inputs("fig1")
    sel = ps.load(args.selection or runs["selection"])
    drop = sel["label"].str.startswith("simd_cut_onpair/") | sel["label"].str.startswith("simd_cut_fsst/")
    d = pm.pd.concat([sel[~drop],
                      ps.load(args.lz4 or runs["lz4"]),
                      ps.load(args.uncompressed or runs["uncompressed"]),
                      ps.load(args.onpair or runs["onpair"]),
                      ps.load(args.fsst or runs["fsst"])], ignore_index=True)
    d = d[d["label"].isin(SYSTEMS) & d["share"].isin([s for s, _ in SHARES]) & (d["dataset"] == DATASET)]
    print(d.groupby(["label", "share"])["gbps"].median().unstack().round(2).to_string())

    pf.save(draw(d), "fig1", args.out)


if __name__ == "__main__":
    main()
