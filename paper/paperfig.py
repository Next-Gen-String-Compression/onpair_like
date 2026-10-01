"""Shared matplotlib style for the paper figures: ACM column width, one type
size for all of them, the Okabe-Ito hues in a fixed order so a kernel keeps its
colour across every figure."""

import os
import re
import tomllib
from pathlib import Path

import matplotlib as mpl
import matplotlib.pyplot as plt

# Measured from the template: acmart sigconf gives \textwidth 506.295 pt and
# \columnwidth 241.147 pt, in TeX points of 1/72.27 in. Every figure is drawn
# at the width it will print at, so LaTeX scales it by one and a label comes
# out at the size it was set in. check_panels.py enforces that.
PT_IN = 1 / 72.27
TEXT_IN = 506.295 * PT_IN
COLUMN_IN = 241.14749 * PT_IN
PREPRINT_ASSETS = Path(__file__).resolve().parents[2] / "substring_search_preprint" / "assets"
INPUTS_PATH = Path(__file__).resolve().parent / "inputs.toml"


def inputs(section):
    """One section of paper/inputs.toml, the pinned inputs of every figure."""
    with open(INPUTS_PATH, "rb") as f:
        return tomllib.load(f)[section]

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
        "axes.labelpad": 2.4,
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
# below the range opens a gap no data can fall into. Linear up to one row of
# 2^20 (9.5e-7, the smallest a column can express) and logarithmic above. The
# linear stretch is a decade wide so that 0 and 10^-6 stay apart in the
# narrowest panel a figure uses.
LINTHRESH = 1e-6
LINSCALE = 1.0


def selectivity_axis(ax, label="row selectivity"):
    """The one selectivity axis every figure draws, so they match."""
    ax.set_xscale("symlog", linthresh=LINTHRESH, linscale=LINSCALE)
    # The suites reach 0.999, so the axis runs to 1 and says so.
    ax.set_xticks([0, 1e-6, 1e-4, 1e-2, 1])
    ax.set_xticklabels(["0", "$10^{-6}$", "$10^{-4}$", "$10^{-2}$", "1"])
    ax.set_xlim(0, 1.3)
    ax.set_xlabel(label)


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


def save_row(panels, name, stems, width, out_dir=None, unit=TEXT_IN):
    """A row of panels on one y axis, `(fig, ax)` each, drawn at `width`:
    only the first keeps its y label, every axes gets the same
    width and the same bottom, and each PDF is cropped to its own content, the
    row as wide as before. Prints the slot each one needs in the LaTeX, as a
    fraction of \\textwidth, or of \\columnwidth when `unit` is COLUMN_IN."""
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


def save_legend(axes, name, ncol, out_dir=None, width=TEXT_IN):
    """The entries of `axes` alone, as `name/legend.pdf`, to sit under the
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
    save(fig, f"{name}/legend", out_dir, bbox_inches=box)
    plt.close(fig)


def save(fig, name, out_dir=None, bbox_inches=None):
    out_dir = Path(out_dir) if out_dir else PREPRINT_ASSETS
    path = out_dir / f"{name}.pdf"
    path.parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(path, bbox_inches=bbox_inches)
    print(f"wrote {path}")
    if preview := os.environ.get("PAPERFIG_PNG"):
        fig.savefig(Path(preview) / f"{name}.png", dpi=300, bbox_inches=bbox_inches)
