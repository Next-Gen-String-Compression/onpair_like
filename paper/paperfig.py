"""Shared matplotlib style for the paper figures: ACM column width, one type
size for all of them, the Okabe-Ito hues in a fixed order so a kernel keeps its
colour across every figure."""

import json
import os
import re
from pathlib import Path

import matplotlib as mpl
import numpy as np
import matplotlib.pyplot as plt
import pandas as pd

import config

# Measured from the template: acmart sigconf gives \textwidth 506.295 pt and
# \columnwidth 241.147 pt, in TeX points of 1/72.27 in. Every figure is drawn
# at the width it will print at, so LaTeX scales it by one and a label comes
# out at the size it was set in. check_panels.py enforces that.
PT_IN = 1 / 72.27
TEXT_IN = 506.295 * PT_IN
COLUMN_IN = 241.14749 * PT_IN
PREPRINT_ASSETS = Path(__file__).resolve().parents[2] / "substring_search_preprint" / "assets"


def inputs(section):
    """One section of config.INPUTS, the pinned inputs of every figure, or of
    the inputs file PAPER_INPUTS names (paper/inputs.py, for make-figures.sh)."""
    if path := os.environ.get("PAPER_INPUTS"):
        return json.loads(Path(path).read_text())[section]
    return config.INPUTS[section]


def read_bandwidth(isa, kib=None):
    """One core's sequential read GB/s on the machine `isa` ran on: at `kib`
    of working set, or at the largest size measured, where it reads memory."""
    d = pd.read_csv(Path(__file__).resolve().parent / inputs("bandwidth")[isa])
    return float(d.loc[d["kib"] == (kib or d["kib"].max()), "seq_gbs"].iloc[0])


def bandwidth_line(ax, gbs, label=None):
    """A thin dotted line at `gbs`, one core's read rate; `label` puts it in the legend."""
    ax.axhline(gbs, color="#222222", linestyle=(0, (1, 1.5)), linewidth=0.9, zorder=1, label=label)


COLOR = {
    "eq_or": "#0072B2",
    "nibble": "#D55E00",
    "range": "#009E73",
    "table": "#CC79A7",
    "bitmap": "#E69F00",
    "linear_seek": "#0072B2",
    "gallop_seek": "#D55E00",
    "walk": "#0072B2",
    "memmem": "#D55E00",
    "kmp": "#009E73",
    "none": "#CC79A7",
}
MARKER = {
    "eq_or": "o", "nibble": "^", "range": "s", "table": "D", "bitmap": "v",
    "linear_seek": "o", "gallop_seek": "^",
    "walk": "o", "memmem": "^", "kmp": "s", "none": "D",
}


# The only two type sizes in the figures: everything a script writes uses one
# of these, never a literal.
BODY_PT = 8.4
SMALL_PT = 7.8


# acmart sigconf sets text in Linux Libertine, math in newtxmath's libertine
# variant and mono in Inconsolata; the figures go through LaTeX with the same
# three packages, so a label is set in the same font as the body text beside it.
TEX_PREAMBLE = "\n".join([
    r"\usepackage[tt=false,type1=true]{libertine}",
    r"\usepackage[varqu]{zi4}",
    r"\usepackage[libertine]{newtxmath}",
])


def style():
    mpl.rcParams.update({
        "text.usetex": True,
        "text.latex.preamble": TEX_PREAMBLE,
        "font.family": "serif",
        "font.size": BODY_PT,
        "axes.labelsize": BODY_PT,
        "axes.titlesize": BODY_PT,
        "legend.fontsize": SMALL_PT,
        "xtick.labelsize": SMALL_PT,
        "ytick.labelsize": SMALL_PT,
        "lines.linewidth": 1.0,
        "lines.markersize": 3,
        "axes.spines.top": False,
        "axes.spines.right": False,
        "axes.grid": True,
        "grid.color": "#E0E0E0",
        "grid.linewidth": 0.4,
        "axes.axisbelow": True,
        "legend.frameon": False,
        "pdf.fonttype": 42,
        "axes.labelpad": 2.0,
        # No margin inside a PDF; the spacing between panels is LaTeX's.
        "figure.constrained_layout.w_pad": 0,
        "figure.constrained_layout.h_pad": 0,
        "savefig.pad_inches": 0,
    })


def figure(ncols=1, height=1.8):
    style()
    fig, axes = plt.subplots(1, ncols, figsize=(COLUMN_IN, height), constrained_layout=True)
    return fig, axes


# One panel per file, the grid assembled in LaTeX with subfigure: a panel keeps
# its own aspect ratio whatever the column it lands in, and a figure can be
# re-arranged without re-running anything. A panel is drawn at the width it
# will be printed at, so `width=\linewidth` in LaTeX scales it by one and the
# type stays the size it was set in; a panel drawn narrower than its slot comes
# out with labels larger again than that.
PANEL_4UP = 0.23 * TEXT_IN
PANEL_3UP = 0.32 * TEXT_IN
PANEL_2UP = 0.49 * TEXT_IN


def panel(width=PANEL_3UP, height=1.55):
    style()
    fig, ax = plt.subplots(figsize=(width, height), constrained_layout=True)
    return fig, ax


# A selectivity axis is symlog, not log. A query matching no row has a
# selectivity of 0, which a log axis cannot place; parking it at a stand-in
# below the range opens a gap no data can fall into. Linear up to 10^-5, the
# lowest bracket, and logarithmic above.
LINTHRESH = 1e-5
LINSCALE = 1.0


def fastest(d, keys, col):
    """The row with the lowest `col` per `keys`, one row each. idxmin returns
    labels, so the index is renumbered first: on rows concatenated without
    ignore_index a label names several rows and every one would be kept."""
    d = d.reset_index(drop=True)
    best = d.loc[d.groupby(keys)[col].idxmin()]
    if best.duplicated(keys).any():
        raise SystemExit(f"fastest: more than one row per {keys}")
    return best


def selectivity_bracket(sel):
    """The bracket every figure bins selectivity into, as its upper edge: 0, then
    decades from below 10^-5 up to 0.1-1, the rows of the matrices."""
    sel = np.asarray(sel, dtype=float)
    out = np.zeros_like(sel)
    live = sel > 0
    out[live] = np.clip(10 ** (np.floor(np.log10(sel[live])) + 1), 1e-5, 1)
    return out


def share_ticks(ax, ticks, labelled):
    """x ticks at the shares of rows `ticks`, those in `labelled` written as
    10^k or, under config.PERCENT_TICKS, as a percentage."""
    def text(t):
        if config.PERCENT_TICKS:
            return f"{100 * t:g}\\%"
        if t == 0 or t == 1:
            return f"{t:g}"
        return f"$10^{{{round(np.log10(t))}}}$"
    ax.set_xticks(ticks, [text(t) if t in labelled else "" for t in ticks])


def selectivity_axis(ax, label="needle selectivity"):
    """The one selectivity axis every figure draws, so they match."""
    ax.set_xscale("symlog", linthresh=LINTHRESH, linscale=LINSCALE)
    share_ticks(ax, [0, 1e-5, 1e-4, 1e-3, 1e-2, 1e-1, 1], [0, 1e-4, 1e-2, 1])
    ax.xaxis.set_minor_locator(mpl.ticker.NullLocator())
    ax.set_xlim(0, 1.3)
    ax.set_xlabel(label)


def y_axis(axes, lim=None, ticks=None, fmt=None, minor=True, figure=None, top=None):
    """The y axis of one panel or of a row of panels that share it, called
    after plotting. Log over `lim`, with major ticks at `ticks`; under
    config.LINEAR_Y linear from 0 to the row's highest value, capped at
    `lim`'s top, with matplotlib's ticks. `fmt` formats a tick label; a
    `figure` in config.LOG_FIGURES stays log; `top` fixes the top in either
    scale."""
    axes = [axes] if isinstance(axes, mpl.axes.Axes) else list(axes)
    linear = config.LINEAR_Y and figure not in config.LOG_FIGURES
    if linear:
        if top is None:
            data = max(ax.dataLim.y1 for ax in axes)
            # Up to the tick at or above the data, so the top is labelled.
            ticks = mpl.ticker.MaxNLocator(nbins=5, steps=[1, 2, 2.5, 5, 10]).tick_values(0, 1.02 * data)
            top = min(t for t in ticks if t >= 1.02 * data)
            if lim is not None:
                top = min(top, lim[1])
    for ax in axes:
        if linear:
            ax.set_ylim(0, top)
        else:
            ax.set_yscale("log")
            if lim is not None:
                ax.set_ylim(*lim)
            if top is not None:
                ax.set_ylim(top=top)
            if ticks is not None:
                ax.yaxis.set_major_locator(mpl.ticker.FixedLocator(ticks))
            if not minor:
                ax.yaxis.set_minor_locator(mpl.ticker.NullLocator())
        if fmt is not None:
            ax.yaxis.set_major_formatter(mpl.ticker.FuncFormatter(lambda v, _: fmt.format(v)))


# The plot area of a column-wide figure whose x axis lines up with another's:
# inches left and right of the axes, room for the widest y label and the last
# x tick label.
COLUMN_AXES_X = (0.37, 0.06)


def align_column_x(fig, axes):
    """Fix the axes of `fig` to COLUMN_AXES_X across the column, keeping the
    heights the layout gave them, so stacked figures share one x axis."""
    fig.canvas.draw()
    inv = fig.dpi_scale_trans.inverted()
    w = fig.get_figwidth()
    ink = fig.get_tightbbox()
    box = axes[0].get_window_extent().transformed(inv)
    print(f"natural x margins {box.x0 - ink.x0:.3f} left, {ink.x1 - box.x1:.3f} right (in)")
    print(f"legend above it: \\hspace{{{(COLUMN_AXES_X[0] - COLUMN_AXES_X[1]) / COLUMN_IN:.4f}\\columnwidth}}")
    fig.set_layout_engine("none")
    left, right = COLUMN_AXES_X
    for ax in axes:
        p = ax.get_position()
        ax.set_position([left / w, p.y0, 1 - (left + right) / w, p.height])
    fig.canvas.draw()
    ink = fig.get_tightbbox()
    if ink.x0 < -0.01 or ink.x1 > w + 0.01:
        print(f"warning: ink {ink.x0:.3f}..{ink.x1:.3f} outside the {w:.3f} in figure")


def slug(text):
    """A panel's file name from its label: `x86, AVX-512` -> `x86-avx512`.
    Panels are named for what they show, so a figure's folder reads as its
    contents rather than as 1, 2, 3."""
    out = re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")
    return out.replace("avx-512", "avx512")


def clear_figure(name, out_dir=None):
    """Drop a figure's folder before regenerating it. A run with fewer panels
    than the last one would otherwise leave the extras behind, and LaTeX would
    include a stale panel beside fresh ones."""
    folder = (Path(out_dir) if out_dir else PREPRINT_ASSETS) / name
    if folder.is_dir():
        for old in folder.glob("*.pdf"):
            old.unlink()


def save_panel(fig, name, index, out_dir=None):
    """`name/1.pdf`, `name/2.pdf`, ... — a folder per multi-panel figure, so
    `assets/` lists figures rather than dozens of panels."""
    save(fig, f"{name}/{index}", out_dir)
    plt.close(fig)


# The gap between the plot areas of two panels side by side, in inches,
# whether or not the second numbers its y axis.
PANEL_GAP = 0.25


def save_row(panels, name, stems, width, out_dir=None, unit=TEXT_IN, gap=PANEL_GAP):
    """A row of panels on one y axis, `(fig, ax)` each, drawn at `width`:
    only the first keeps its y label, every axes gets the same
    width and the same bottom, and each PDF is cropped to its own content, the
    row as wide as before. Prints the slot each one needs in the LaTeX, as a
    fraction of \\textwidth, or of \\columnwidth when `unit` is COLUMN_IN.
    Placed edge to edge, two neighbours' plot areas are at least `gap` apart;
    0 for panels that are stacked or meant to touch."""
    for _fig, ax in panels[1:]:
        ax.set_ylabel("")
    margins = []
    for fig, ax in panels:
        fig.canvas.draw()
        inv = fig.dpi_scale_trans.inverted()
        box = ax.get_window_extent().transformed(inv)
        ink = ax.get_tightbbox().transformed(inv)
        tick = ax.yaxis.get_major_ticks()[0].tick1line.get_markersize() / 72
        margins.append((max(box.x0 - ink.x0, tick), max(ink.x1 - box.x1, 0),
                        max(box.y0 - ink.y0, 0), max(ink.y1 - box.y1, 0)))
    for i in range(1, len(margins)):
        left, right, b, t = margins[i]
        margins[i] = (max(left, gap - margins[i - 1][1]), right, b, t)
    bottom = max(m[2] for m in margins)
    top = max(m[3] for m in margins)
    axes_w = (len(panels) * width - sum(m[0] + m[1] for m in margins)) / len(panels)
    for (fig, ax), (left, right, _b, _t), stem in zip(panels, margins, stems):
        fig.set_layout_engine("none")
        if ax.get_aspect() == "auto":
            axes_h = fig.get_figheight() - bottom - top
        else:
            axes_h = axes_w * ax.get_data_ratio() * ax.get_aspect()
        w, h = left + axes_w + right, bottom + axes_h + top
        fig.set_size_inches(w, h)
        ax.set_position([left / w, bottom / h, axes_w / w, axes_h / h])
        print(f"{name}/{stem}: {w / unit:.4f}\\{'columnwidth' if unit == COLUMN_IN else 'textwidth'}")
        save_panel(fig, name, stem, out_dir)


def save_legend(axes, name, ncol, out_dir=None, width=TEXT_IN, stem="legend"):
    """The entries of `axes` alone, as `name/<stem>.pdf`, to sit under the
    row. Several axes pool their entries, first label wins. The strip is
    cropped to the entries on all sides, so LaTeX includes it at its natural
    size, `\\includegraphics{name/legend}`, in a one- or two-column figure
    alike; `width` is only the canvas it is laid out on."""
    if not isinstance(axes, (list, tuple)):
        axes = [axes]
    handles, labels = [], []
    for ax in axes:
        for handle, label in zip(*ax.get_legend_handles_labels()):
            if label not in labels:
                handles.append(handle)
                labels.append(label)
    if not handles:
        return
    style()
    fig = plt.figure(figsize=(width, 0.4))
    # A strip is included at its natural size, so one wider than the text
    # overfulls the line: close the gaps between entries until it fits.
    for spacing in (2.0, 1.4, 1.0, 0.7, 0.5):
        legend = fig.legend(handles, labels, loc="center", ncol=ncol, handlelength=1.4,
                            fontsize=SMALL_PT, borderpad=0, borderaxespad=0,
                            columnspacing=spacing, handletextpad=0.5)
        fig.canvas.draw()
        box = legend.get_window_extent().transformed(fig.dpi_scale_trans.inverted())
        if box.width <= width:
            break
        legend.remove()
    else:
        raise SystemExit(f"{name}/{stem}: {ncol} columns are {box.width:.2f}in wide, "
                         f"{width:.2f}in fit; shorten a label or add a column")
    save(fig, f"{name}/{stem}", out_dir, bbox_inches=box)
    plt.close(fig)


def save_grouped_legend(axes, groups, name, out_dir=None, width=TEXT_IN, stem="legend", inline=False):
    """Like `save_legend`, with the entries in titled blocks side by side: each
    block its title over its entries in one row, or with `inline` before them
    on the same row. `groups` is
    `[(title, [(label, shown), ...]), ...]`, `label` the entry's label in `axes`
    and `shown` what the block prints; entries not in `axes` are left out, and
    a block left empty."""
    if not isinstance(axes, (list, tuple)):
        axes = [axes]
    found = {}
    for ax in axes:
        for handle, label in zip(*ax.get_legend_handles_labels()):
            found.setdefault(label, handle)
    blocks = [(title, [(found[label], shown) for label, shown in entries if label in found])
              for title, entries in groups]
    blocks = [(title, entries) for title, entries in blocks if entries]
    style()
    fig = plt.figure(figsize=(width, 0.6))
    to_in = fig.dpi_scale_trans.inverted()
    # Blocks laid left to right; close the gaps between them and between
    # entries until the strip fits the text.
    for gap, spacing in ((0.30, 1.0), (0.22, 0.8), (0.15, 0.6), (0.10, 0.4)):
        legends, x = [], 0.0
        for title, entries in blocks:
            if inline:
                head = fig.text(x / width, 0.5, title, ha="left", va="center", fontsize=SMALL_PT)
                fig.canvas.draw()
                x += head.get_window_extent().transformed(to_in).width + 0.06
                legends.append(head)
            legend = fig.legend([h for h, _ in entries], [s for _, s in entries],
                                title=None if inline else title,
                                ncol=len(entries), loc="center left" if inline else "upper left",
                                alignment="left",
                                bbox_to_anchor=(x / width, 0.5 if inline else 1.0), bbox_transform=fig.transFigure,
                                title_fontsize=SMALL_PT, fontsize=SMALL_PT, handlelength=1.4,
                                borderpad=0, borderaxespad=0, handletextpad=0.5,
                                columnspacing=spacing, labelspacing=0.15)
            fig.canvas.draw()
            x += legend.get_window_extent().transformed(to_in).width + gap
            legends.append(legend)
        box = mpl.transforms.Bbox.union([lg.get_window_extent().transformed(to_in) for lg in legends])
        if box.width <= width:
            break
        for legend in legends:
            legend.remove()
    else:
        raise SystemExit(f"{name}/{stem}: the blocks are {box.width:.2f}in wide, {width:.2f}in fit")
    save(fig, f"{name}/{stem}", out_dir, bbox_inches=box)
    plt.close(fig)


def save(fig, name, out_dir=None, bbox_inches=None):
    out_dir = Path(out_dir) if out_dir else PREPRINT_ASSETS
    path = out_dir / f"{name}.pdf"
    path.parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(path, bbox_inches=bbox_inches)
    print(f"wrote {path}")
    if preview := os.environ.get("PAPERFIG_PNG"):
        fig.savefig(Path(preview) / f"{name}.png", dpi=300, bbox_inches=bbox_inches)
