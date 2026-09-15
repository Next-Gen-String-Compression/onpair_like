"""Shared matplotlib style for the paper figures: ACM column width, 7 pt
type, the Okabe-Ito hues in a fixed order so a kernel keeps its colour across
every figure."""

import os
from pathlib import Path

import matplotlib as mpl
import matplotlib.pyplot as plt
from matplotlib.transforms import Bbox

# Measured from the template: acmart sigconf gives \textwidth 506.295 pt and
# \columnwidth 241.147 pt, in TeX points of 1/72.27 in. Every figure is drawn
# at the width it will print at, so LaTeX scales it by one and 7 pt type comes
# out 7 pt. check_panels.py enforces that.
PT_IN = 1 / 72.27
TEXT_IN = 506.295 * PT_IN
COLUMN_IN = 241.14749 * PT_IN
PREPRINT_ASSETS = Path(__file__).resolve().parents[2] / "substring_search_preprint" / "assets"

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
BODY_PT = 7
SMALL_PT = 6.5


def style():
    mpl.rcParams.update({
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
    })


def figure(ncols=1, height=1.8):
    style()
    fig, axes = plt.subplots(1, ncols, figsize=(COLUMN_IN, height), constrained_layout=True)
    return fig, axes


# One panel per file, the grid assembled in LaTeX with subfigure: a panel keeps
# its own aspect ratio whatever the column it lands in, and a figure can be
# re-arranged without re-running anything. A panel is drawn at the width it
# will be printed at, so `width=\linewidth` in LaTeX scales it by one and the
# 7 pt type stays 7 pt; a panel drawn narrower than its slot comes out with
# labels larger than the body text.
PANEL_3UP = 0.32 * TEXT_IN
PANEL_2UP = 0.49 * TEXT_IN


def panel(width=PANEL_3UP, height=1.55):
    style()
    fig, ax = plt.subplots(figsize=(width, height), constrained_layout=True)
    return fig, ax


def save_panel(fig, name, index, out_dir=None):
    """`name/1.pdf`, `name/2.pdf`, ... — a folder per multi-panel figure, so
    `assets/` lists figures rather than dozens of panels."""
    save(fig, f"{name}/{index}", out_dir)
    plt.close(fig)


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
    legend = fig.legend(handles, labels, loc="center", ncol=ncol, handlelength=1.4,
                        fontsize=SMALL_PT, borderpad=0, borderaxespad=0)
    fig.canvas.draw()
    box = legend.get_window_extent().transformed(fig.dpi_scale_trans.inverted())
    crop = Bbox([[box.x0 - 0.01, box.y0 - 0.01], [box.x1 + 0.01, box.y1 + 0.01]])
    save(fig, f"{name}/legend", out_dir, bbox_inches=crop)
    plt.close(fig)


def save(fig, name, out_dir=None, bbox_inches=None):
    out_dir = Path(out_dir) if out_dir else PREPRINT_ASSETS
    path = out_dir / f"{name}.pdf"
    path.parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(path, bbox_inches=bbox_inches)
    print(f"wrote {path}")
    if preview := os.environ.get("PAPERFIG_PNG"):
        fig.savefig(Path(preview) / f"{name}.png", dpi=300, bbox_inches=bbox_inches)
