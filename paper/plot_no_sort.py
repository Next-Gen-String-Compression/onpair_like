# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "duckdb>=1.1", "numpy", "pyarrow>=17", "xxhash>=3"]
# ///
"""The SIMD-cut of OnPair12, OnPair16 and FSST with one switch of the debug candidates
off, GB/s against needle selectivity pooled over the four columns, as micro-arch
draws it, on NEON. Both lines of a system come from the one specs/ablation/<size>.toml
run; the default config is the solid line. Per selectivity bracket the median.

    no-sort       sorted symbol table against unsorted ("sorted": false)
    uniform-freq  frequency counts against uniform frequencies ("uniform": true)

    uv run paper/plot_no_sort.py [--run RUN] [--out DIR]

Writes `no-sort/{no-sort,uniform-freq}.pdf`, one column wide together, only the
first with a y label, and `no-sort/legend.pdf` for the systems' colours; each
panel's own legend names the line styles.
"""

import argparse

import config
import paperfig as pf
import plot_micro as pm
from config import COLOR, SCHEME

# (label of the default config, scheme, colour, marker), the micro figures' colours
# and markers.
SYSTEMS = [
    ("onpair_spiral_debug/pf_walk@bits=12,prune=1.0", SCHEME["onpair12"], COLOR["onpair12-cut"], "s"),
    ("onpair_spiral_debug/pf_walk@bits=16,prune=1.0", SCHEME["onpair16"], COLOR["onpair16-cut"], "o"),
    ("fsst_prefilter_debug/fsst-prefilter", SCHEME["fsst"], COLOR["fsst-cut"], "^"),
]
# Per figure, the legend names of the default and the variant, and the config key the
# variant adds to the label: the default solid and filled, the variant dashed and open.
ARMS = {
    "no-sort": (["sorted", "unsorted"], "sorted=False"),
    "uniform-freq": (["counts", "uniform"], "uniform=True"),
}
STYLES = [("-", None), ("--", "white")]


def labels(figure, label):
    """The default's label and the variant's: the switch joins the config suffix."""
    _, key = ARMS[figure]
    return [label, label + ("," if "@" in label else "@") + key]


def draw(figure, run):
    fig, ax = pf.panel(width=0.5 * pf.COLUMN_IN)
    drawn = []
    d = pm.selectivity_x(run)
    names, _ = ARMS[figure]
    for label, scheme, color, marker in SYSTEMS:
        for arm, line, (style, face) in zip(names, labels(figure, label), STYLES):
            m = d[d["label"] == line].groupby("sel")["gbps"].median()
            drawn += [m.min(), m.max()]
            ax.plot(m.index, m.values, color=color, marker=marker, linestyle=style,
                    markerfacecolor=face or color)
            print(scheme, arm, "median", m.round(1).to_dict())
    for arm, (style, face) in zip(names, STYLES):
        ax.plot([], [], color="#555555", marker="o", linestyle=style, markerfacecolor=face or "#555555", label=arm)
    pf.selectivity_axis(ax)
    # Linear, from 0 to the figure's fixed top in config.Y_TOP, else over
    # the values drawn and no further.
    if figure in config.Y_TOP:
        ax.set_ylim(0, config.Y_TOP[figure][1])
    else:
        bottom, top = min(drawn), max(drawn)
        margin = 0.03 * (top - bottom)
        ax.set_ylim(bottom - margin, top + margin)
    ax.set_ylabel("uncompressed GB/s")
    ax.legend(loc="upper right", handlelength=2.2, labelspacing=0.1, borderaxespad=0.2, borderpad=0.1)
    return fig, ax


def save_legend(out):
    """`no-sort/legend`, the systems of both figures, which share it."""
    fig, ax = pf.panel(width=pf.COLUMN_IN)
    for _, scheme, color, marker in SYSTEMS:
        ax.plot([], [], color=color, marker=marker, label=scheme)
    groups = [(config.METHOD["cut"], [(scheme, scheme) for _, scheme, _, _ in SYSTEMS])]
    pf.save_grouped_legend(ax, groups, "no-sort", out, width=pf.COLUMN_IN, inline=True)
    pf.plt.close(fig)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", default=None)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    d = pm.load(args.run or pf.inputs("ablation")["run"])
    d = d[d["chunk_rows"] == pm.ROW_GROUP]
    pf.clear_figure("no-sort", args.out)
    panels = [draw(figure, d) for figure in ARMS]
    pf.save_row(panels, "no-sort", list(ARMS), 0.5 * pf.COLUMN_IN, args.out, unit=pf.COLUMN_IN)
    save_legend(args.out)


if __name__ == "__main__":
    main()
