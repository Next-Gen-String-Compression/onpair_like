#!/usr/bin/env python3
"""One chart, one SVG, in the house style of the landscape figures.

The landscape and report scripts each grew their own plotter, welded to the
shape of the data they happened to load. This one is deliberately not: it takes
a table, a column for x, a column for y, and optionally a column to split into
series, and writes a single panel. Nothing in here knows what a dataset, a
selectivity, or a prefilter is.

Two ways in. From a shell, against a CSV or JSON the harness already wrote:

    python analysis/plot.py --data results.csv \\
        --x needle_len --y plan_us --series dict_bits --series-label "{} bits" \\
        --yscale log --title "Planning cost" --ylabel "Median plan (us)" \\
        --out figures/plan-vs-needle.svg

From Python, when the numbers come from somewhere a CLI cannot reach:

    from plot import Figure
    fig = Figure(title="Planning cost", xlabel="Needle bytes", yscale="log")
    fig.add("16 bits", [(1, 1162.0), (8, 240.0), (16, 231.0)])
    fig.write("figures/plan-vs-needle.svg")

The style — white ground, Okabe-Ito series, a title/subtitle block, a legend
row above the panel and a footnote line under it — is copied from
`landscape.py` rather than shared with it, so changing one cannot silently
restyle the other.
"""

import argparse
import csv
import html
import json
import math
import sys
from pathlib import Path

# The Okabe-Ito palette, paired with a distinct marker so the series stay
# separable in greyscale and to a colourblind reader. Order is the assignment
# order; the wine and indigo tails only appear on figures with many series.
PALETTE = [
    {"color": "#0072B2", "marker": "circle"},
    {"color": "#D55E00", "marker": "triangle"},
    {"color": "#009E73", "marker": "square"},
    {"color": "#CC79A7", "marker": "diamond"},
    {"color": "#E69F00", "marker": "hexagon"},
    {"color": "#56B4E9", "marker": "plus"},
    {"color": "#882255", "marker": "cross"},
    {"color": "#332288", "marker": "circle", "open": True},
]

PAPER = "#FFFFFF"
INK = "#202124"
MUTED = "#5F6368"
TICK_INK = "#6B7075"
AXIS = "#9AA0A6"
GRID = "#ECEFF1"
GRID_FAINT = "#F0F1F2"
FONT_STACK = '-apple-system,BlinkMacSystemFont,"Segoe UI",Arial,sans-serif'

# Legend text is measured, not laid out: at font-size 12 in the stack above,
# this many pixels per character is close enough to keep entries from colliding.
CHAR_PX = 6.55


def esc(value):
    return html.escape(str(value), quote=True)


def dash_attr(style):
    """A series' `stroke-dasharray`, or nothing when it draws solid."""
    return f' stroke-dasharray="{style["dash"]}"' if style.get("dash") else ""


def styled_marker(style, x, y, size, stroke_width=1.2):
    """A series' marker, honouring its `open` flag (white fill, ink outline)."""
    if style.get("open"):
        return marker_svg(kind=style["marker"], x=x, y=y, size=size, color=PAPER,
                          stroke=style["color"], stroke_width=max(1.7, stroke_width + 0.7))
    return marker_svg(style["marker"], x, y, size, style["color"], stroke_width=stroke_width)


def marker_svg(kind, x, y, size, color, opacity=1.0, stroke=PAPER, stroke_width=1.2):
    common = f'fill="{color}" fill-opacity="{opacity:.3f}" stroke="{stroke}" stroke-width="{stroke_width}"'
    if kind == "circle":
        return f'<circle cx="{x:.2f}" cy="{y:.2f}" r="{size:.2f}" {common}/>'
    if kind == "square":
        return f'<rect x="{x-size:.2f}" y="{y-size:.2f}" width="{2*size:.2f}" height="{2*size:.2f}" rx="0.7" {common}/>'
    if kind == "diamond":
        return f'<path d="M {x:.2f} {y-size-0.5:.2f} L {x+size+0.5:.2f} {y:.2f} L {x:.2f} {y+size+0.5:.2f} L {x-size-0.5:.2f} {y:.2f} Z" {common}/>'
    if kind == "triangle":
        return f'<path d="M {x:.2f} {y-size-0.8:.2f} L {x+size+0.8:.2f} {y+size:.2f} L {x-size-0.8:.2f} {y+size:.2f} Z" {common}/>'
    if kind == "cross":
        return (
            f'<path d="M {x-size:.2f} {y-size:.2f} L {x+size:.2f} {y+size:.2f} '
            f'M {x+size:.2f} {y-size:.2f} L {x-size:.2f} {y+size:.2f}" '
            f'fill="none" stroke="{color}" stroke-opacity="{opacity:.3f}" stroke-width="2" stroke-linecap="round"/>'
        )
    if kind == "plus":
        arm = size + 0.9
        return (
            f'<path d="M {x-arm:.2f} {y:.2f} L {x+arm:.2f} {y:.2f} '
            f'M {x:.2f} {y-arm:.2f} L {x:.2f} {y+arm:.2f}" '
            f'fill="none" stroke="{color}" stroke-opacity="{opacity:.3f}" stroke-width="2.2" stroke-linecap="round"/>'
        )
    if kind == "hexagon":
        r = size + 0.4
        points = " ".join(
            f"{x + r*math.cos(math.radians(60*i)):.2f},{y + r*math.sin(math.radians(60*i)):.2f}"
            for i in range(6)
        )
        return f'<polygon points="{points}" {common}/>'
    raise ValueError(kind)


def fmt_num(value):
    """A tick label: integers as integers, everything else at three digits."""
    if value == int(value) and abs(value) < 1e15:
        magnitude = abs(int(value))
        return f"{int(value):,}" if magnitude >= 10_000 else str(int(value))
    return f"{value:.3g}"


def linear_ticks(lo, hi):
    """Bounds snapped out to a round 1/2/2.5/5 x 10^k step, and that step's ticks.

    Returns `(lo, hi, ticks)` with the bounds widened to land on ticks, so the
    panel's top and bottom edges are labelled values rather than whatever the
    data happened to reach.
    """
    if not math.isfinite(lo) or not math.isfinite(hi):
        lo, hi = 0.0, 1.0
    if hi <= lo:
        hi = lo + (abs(lo) * 0.1 or 1.0)
    span = hi - lo
    exponent = math.floor(math.log10(span))
    for step in sorted(factor * 10.0 ** e
                       for e in range(exponent - 1, exponent + 2)
                       for factor in (1, 2, 2.5, 5)):
        first = math.floor(lo / step + 1e-9)
        last = math.ceil(hi / step - 1e-9)
        if last - first <= 8:
            ticks = [(first + i) * step for i in range(last - first + 1)]
            return ticks[0], ticks[-1], ticks
    return lo, hi, [lo, hi]


def log_ticks(lo, hi):
    """Bounds snapped to a round 1/2/5 x 10^k, and the ticks between them.

    Whole decades are the obvious bounds and they waste the panel: a series
    living between 1.2 ms and 2.7 ms, drawn on a 1 ms to 100 ms axis, is a flat
    line in the bottom fifth. Snapping to the 1/2/5 ladder keeps a narrow span
    readable. A span already wide enough not to care falls back to decades,
    where 1/2/5 would only crowd the labels.
    """
    lo = max(lo, 1e-300)
    hi = max(hi, lo * (1 + 1e-9))
    low_exponent = math.floor(math.log10(lo))
    high_exponent = math.ceil(math.log10(hi))
    ladder = sorted(factor * 10.0 ** e
                    for e in range(low_exponent - 1, high_exponent + 2)
                    for factor in (1, 2, 5))
    lo = max(rung for rung in ladder if rung <= lo * (1 + 1e-9))
    hi = min(rung for rung in ladder if rung >= hi * (1 - 1e-9))
    if hi <= lo * (1 + 1e-6):
        hi = lo * 10.0
    if math.log10(hi) - math.log10(lo) > 3:
        lo = 10.0 ** math.floor(math.log10(lo))
        hi = 10.0 ** math.ceil(math.log10(hi))
        factors = (1,)
    else:
        factors = (1, 2, 5)
    ticks = sorted(factor * 10.0 ** e
                   for e in range(math.floor(math.log10(lo)), math.ceil(math.log10(hi)) + 1)
                   for factor in factors)
    return lo, hi, [t for t in ticks if lo * (1 - 1e-9) <= t <= hi * (1 + 1e-9)]


class Figure:
    """A single panel: axes, an optional legend, and one line per series.

    Sizes are pixels in a fixed viewBox. The left margin adapts to the widest y
    tick label and the top to how many rows the legend needs, so a figure with
    long labels does not clip and one with none does not leave a gap.
    """

    def __init__(self, *, title="", subtitle="", xlabel="", ylabel="", note="",
                 width=900, height=520, xscale="linear", yscale="linear",
                 xlim=None, ylim=None, hline=None, clamp=False):
        self.title = title
        self.subtitle = subtitle
        self.xlabel = xlabel
        self.ylabel = ylabel
        self.note = note
        self.width = width
        self.height = height
        self.xscale = xscale
        self.yscale = yscale
        self.xlim = xlim
        self.ylim = ylim
        self.hline = hline
        self.clamp = clamp
        self.series = []
        self.categories = None

    def add(self, label, points, *, style=None, line=True, markers=True, cloud=()):
        """Add one series, drawn in palette order unless `style` overrides it.

        `points` are the `(x, y)` the line connects; `cloud` are raw `(x, y)`
        drawn as faint dots behind it, for showing the spread an aggregate hid.
        An x may be a number or a string — strings make the axis categorical.
        """
        base = PALETTE[len(self.series) % len(PALETTE)]
        self.series.append({
            "label": label,
            "points": list(points),
            "cloud": list(cloud),
            "style": {**base, **(style or {})},
            "line": line,
            "markers": markers,
        })
        return self

    def _resolve_x(self):
        """Map every x to a number, inventing positions for categorical labels."""
        values = [p[0] for s in self.series for p in list(s["points"]) + list(s["cloud"])]
        if all(isinstance(v, (int, float)) and not isinstance(v, bool) for v in values):
            self.categories = None
            return
        seen = []
        for value in values:
            if value not in seen:
                seen.append(value)
        try:
            seen.sort(key=float)
        except (TypeError, ValueError):
            seen.sort(key=str)
        self.categories = seen
        index = {value: position for position, value in enumerate(seen)}
        for entry in self.series:
            entry["points"] = [(index[x], y) for x, y in entry["points"]]
            entry["cloud"] = [(index[x], y) for x, y in entry["cloud"]]

    def _drop_nonpositive(self):
        """Log axes cannot draw a zero; say which points went and why."""
        dropped = 0
        for entry in self.series:
            for key in ("points", "cloud"):
                kept = [
                    (x, y) for x, y in entry[key]
                    if (self.xscale != "log" or x > 0) and (self.yscale != "log" or y > 0)
                ]
                dropped += len(entry[key]) - len(kept)
                entry[key] = kept
        if dropped:
            print(f"plot: dropped {dropped} non-positive point(s) off the log axis",
                  file=sys.stderr)

    def _bounds(self, index, scale, limit):
        values = [p[index] for s in self.series for p in list(s["points"]) + list(s["cloud"])]
        if self.hline is not None and index == 1:
            values.append(self.hline)
        if not values:
            values = [0.0, 1.0]
        lo = limit[0] if limit and limit[0] is not None else min(values)
        hi = limit[1] if limit and limit[1] is not None else max(values)
        if scale == "log":
            lo, hi, ticks = log_ticks(lo, hi)
        else:
            lo, hi, ticks = linear_ticks(lo, hi)
        if limit:
            # An explicit limit is the axis, not a hint: keep the ticks it spans.
            lo = limit[0] if limit[0] is not None else lo
            hi = limit[1] if limit[1] is not None else hi
            ticks = [t for t in ticks if lo <= t <= hi] or [lo, hi]
        return lo, hi, ticks

    def _legend_rows(self, available):
        """Pack legend entries into rows, each an entry list and its total width."""
        rows, row, used = [], [], 0.0
        for entry in self.series:
            span = 52 + CHAR_PX * len(entry["label"])
            if row and used + span > available:
                rows.append(row)
                row, used = [], 0.0
            row.append(entry)
            used += span
        if row:
            rows.append(row)
        return rows

    def svg(self):
        self._resolve_x()
        self._drop_nonpositive()

        xlo, xhi, xticks = self._bounds(0, self.xscale, self.xlim)
        ylo, yhi, yticks = self._bounds(1, self.yscale, self.ylim)
        if self.categories is not None:
            # Categories sit on integer positions; give the end ones breathing room.
            xlo, xhi = -0.5, len(self.categories) - 0.5
            xticks = list(range(len(self.categories)))

        widest = max(len(fmt_num(t)) for t in yticks)
        left = max(64.0, 30 + 6.4 * widest + (18 if self.ylabel else 0))
        right = 28.0

        cursor = 0.0
        y_title = y_subtitle = None
        if self.title:
            y_title = 30.0
            cursor = 30.0
        if self.subtitle:
            y_subtitle = cursor + 23 if cursor else 26.0
            cursor = y_subtitle
        legend_rows = self._legend_rows(self.width - left - right) if self.series else []
        show_legend = bool(legend_rows) and any(s["label"] for s in self.series)
        legend_top = None
        if show_legend:
            legend_top = cursor + 27 if cursor else 22.0
            cursor = legend_top + 22 * (len(legend_rows) - 1)
        top = cursor + 28 if cursor else 34.0
        bottom = self.height - (78 if self.xlabel else 58)
        if self.note:
            bottom = min(bottom, self.height - 78)
        panel_w = self.width - left - right
        panel_h = bottom - top

        def x_pos(value):
            value = min(xhi, max(xlo, value)) if self.clamp else value
            if self.xscale == "log":
                fraction = (math.log10(value) - math.log10(xlo)) / (math.log10(xhi) - math.log10(xlo))
            else:
                fraction = (value - xlo) / (xhi - xlo) if xhi > xlo else 0.5
            return left + fraction * panel_w

        def y_pos(value):
            value = min(yhi, max(ylo, value)) if self.clamp else value
            if self.yscale == "log":
                fraction = (math.log10(value) - math.log10(ylo)) / (math.log10(yhi) - math.log10(ylo))
            else:
                fraction = (value - ylo) / (yhi - ylo) if yhi > ylo else 0.5
            return bottom - fraction * panel_h

        out = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{self.width}" '
            f'height="{self.height}" viewBox="0 0 {self.width} {self.height}">',
            f'<rect width="100%" height="100%" fill="{PAPER}"/>',
            f'<style>text{{font-family:{FONT_STACK};fill:{INK}}}</style>',
        ]
        if y_title is not None:
            out.append(f'<text x="{left:.0f}" y="{y_title:.0f}" font-size="22" font-weight="650">'
                       f'{esc(self.title)}</text>')
        if y_subtitle is not None:
            out.append(f'<text x="{left:.0f}" y="{y_subtitle:.0f}" font-size="12.5" fill="{MUTED}">'
                       f'{esc(self.subtitle)}</text>')

        if show_legend:
            for row_index, row in enumerate(legend_rows):
                y = legend_top + 22 * row_index
                x = left
                for entry in row:
                    style = entry["style"]
                    if entry["line"]:
                        out.append(f'<line x1="{x:.1f}" y1="{y:.1f}" x2="{x+27:.1f}" y2="{y:.1f}" '
                                   f'stroke="{style["color"]}" stroke-width="2.5"{dash_attr(style)}/>')
                    if entry["markers"]:
                        out.append(styled_marker(style, x + 13.5, y, 4.2))
                    out.append(f'<text x="{x+34:.1f}" y="{y+4:.1f}" font-size="12">'
                               f'{esc(entry["label"])}</text>')
                    x += 52 + CHAR_PX * len(entry["label"])

        if self.ylabel:
            out.append(f'<text transform="translate(20 {(top+bottom)/2:.1f}) rotate(-90)" '
                       f'text-anchor="middle" font-size="13" font-weight="600">'
                       f'{esc(self.ylabel)}</text>')

        for tick in yticks:
            y = y_pos(tick)
            out.append(f'<line x1="{left:.1f}" y1="{y:.2f}" x2="{left+panel_w:.1f}" y2="{y:.2f}" '
                       f'stroke="{GRID}" stroke-width="0.7"/>')
            out.append(f'<text x="{left-9:.1f}" y="{y+4:.2f}" text-anchor="end" font-size="10.5" '
                       f'fill="{TICK_INK}">{esc(fmt_num(tick))}</text>')

        for tick in xticks:
            x = x_pos(tick)
            label = self.categories[int(tick)] if self.categories is not None else fmt_num(tick)
            out.append(f'<line x1="{x:.2f}" y1="{top:.1f}" x2="{x:.2f}" y2="{bottom:.1f}" '
                       f'stroke="{GRID_FAINT}" stroke-width="0.8"/>')
            out.append(f'<line x1="{x:.2f}" y1="{bottom:.1f}" x2="{x:.2f}" y2="{bottom+5:.1f}" '
                       f'stroke="{AXIS}"/>')
            out.append(f'<text x="{x:.2f}" y="{bottom+20:.1f}" text-anchor="middle" font-size="10.5" '
                       f'fill="{MUTED}">{esc(label)}</text>')
        out.append(f'<line x1="{left:.1f}" y1="{bottom:.1f}" x2="{left+panel_w:.1f}" y2="{bottom:.1f}" '
                   f'stroke="{AXIS}"/>')

        if self.hline is not None and ylo <= self.hline <= yhi:
            y = y_pos(self.hline)
            out.append(f'<line x1="{left:.1f}" y1="{y:.2f}" x2="{left+panel_w:.1f}" y2="{y:.2f}" '
                       f'stroke="{AXIS}" stroke-width="1.4"/>')

        out.append(f'<defs><clipPath id="panel"><rect x="{left:.1f}" y="{top:.1f}" '
                   f'width="{panel_w:.1f}" height="{panel_h:.1f}"/></clipPath></defs>')
        out.append('<g clip-path="url(#panel)">')
        for entry in self.series:
            style = entry["style"]
            for x, y in entry["cloud"]:
                out.append(f'<circle cx="{x_pos(x):.2f}" cy="{y_pos(y):.2f}" r="1.35" '
                           f'fill="{style["color"]}" opacity="0.14"/>')
            coords = [(x_pos(x), y_pos(y)) for x, y in sorted(entry["points"])]
            if entry["line"] and len(coords) > 1:
                path = " ".join(("M" if i == 0 else "L") + f" {x:.2f} {y:.2f}"
                                for i, (x, y) in enumerate(coords))
                out.append(f'<path d="{path}" fill="none" stroke="{style["color"]}" '
                           f'stroke-width="2.15" stroke-linejoin="round" stroke-linecap="round" '
                           f'opacity="0.94"{dash_attr(style)}/>')
            if entry["markers"]:
                for x, y in coords:
                    out.append(styled_marker(style, x, y, 3.8, stroke_width=1.0))
        out.append('</g>')

        if self.xlabel:
            out.append(f'<text x="{left+panel_w/2:.1f}" y="{bottom+46:.1f}" text-anchor="middle" '
                       f'font-size="12" font-weight="600">{esc(self.xlabel)}</text>')
        if self.note:
            out.append(f'<text x="{left:.0f}" y="{self.height-14}" font-size="10.5" '
                       f'fill="{TICK_INK}">{esc(self.note)}</text>')
        out.append('</svg>')
        return "\n".join(out)

    def write(self, path):
        path = Path(path)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(self.svg())
        return path


def read_table(path):
    """Rows as dicts, from CSV, JSON (a list of objects), or JSON Lines."""
    path = Path(path)
    text = path.read_text()
    if path.suffix == ".jsonl":
        return [json.loads(line) for line in text.splitlines() if line.strip()]
    if path.suffix == ".json":
        data = json.loads(text)
        return data if isinstance(data, list) else data["rows"]
    return list(csv.DictReader(text.splitlines()))


def as_number(value):
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def median(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def bin_points(points, count, collapse, logspace):
    """Collapse a continuous x into `count` buckets of equal width.

    A measurement whose x is a measured quantity rather than a chosen one —
    selectivity, row count, compression ratio — has a different x for every
    sample, so collapsing repeats at one x collapses nothing and the line
    sawtooths through the scatter. Bucketing gives the aggregate something to
    aggregate over.

    Each bucket is drawn at the median of the x values that fell in it, not at
    the bucket's midpoint, so a sparsely populated bucket sits where its data
    actually is.
    """
    xs = [x for x, _ in points]
    lo, hi = min(xs), max(xs)
    if logspace:
        lo, hi = math.log10(lo), math.log10(hi)
    width = (hi - lo) / count
    if width <= 0:
        return [(lo, collapse([y for _, y in points]))]
    buckets = {}
    for x, y in points:
        position = math.log10(x) if logspace else x
        buckets.setdefault(min(count - 1, int((position - lo) / width)), []).append((x, y))
    return [
        (median([x for x, _ in bucket]), collapse([y for _, y in bucket]))
        for _, bucket in sorted(buckets.items())
    ]


def build(rows, args):
    """Group `rows` into series and collapse repeats at the same x.

    Series come from one of two places, because measurements arrive in both
    shapes. A long table names its series in a column (`--series`); a wide one
    — a phase breakdown, say — puts each series in its own column, and naming
    several `--y` makes each of them a line.
    """
    columns = args.y
    grouped = {}
    for row in rows:
        if any(str(row.get(column)) != value for column, value in args.filter):
            continue
        x = row.get(args.x)
        numeric_x = as_number(x)
        x = numeric_x if numeric_x is not None else x
        for column in columns:
            y = as_number(row.get(column))
            if y is None:
                continue
            key = column if len(columns) > 1 else (str(row.get(args.series)) if args.series else "")
            grouped.setdefault(key, []).append((x, y))

    if len(columns) > 1:
        keys = [c for c in columns if c in grouped]
    else:
        try:
            keys = sorted(grouped, key=float)
        except ValueError:
            keys = sorted(grouped)

    fig = Figure(
        title=args.title, subtitle=args.subtitle, note=args.note,
        xlabel=args.xlabel if args.xlabel is not None else args.x,
        ylabel=args.ylabel if args.ylabel is not None else ", ".join(args.y),
        width=args.width, height=args.height,
        xscale=args.xscale, yscale=args.yscale,
        xlim=args.xlim, ylim=args.ylim, hline=args.hline, clamp=args.clamp,
    )
    for key in keys:
        raw = grouped[key]
        if args.agg == "none":
            points = sorted(raw)
        else:
            collapse = median if args.agg == "median" else (lambda v: sum(v) / len(v))
            numeric = all(isinstance(x, (int, float)) for x, _ in raw)
            if args.bin and numeric:
                usable = [(x, y) for x, y in raw if args.xscale != "log" or x > 0]
                points = bin_points(usable, args.bin, collapse, args.xscale == "log")
            else:
                at_x = {}
                for x, y in raw:
                    at_x.setdefault(x, []).append(y)
                points = sorted((x, collapse(ys)) for x, ys in at_x.items())
        named = args.series or len(columns) > 1
        fig.add(args.series_label.format(key) if named else "",
                points, line=not args.no_line,
                cloud=raw if args.points and args.agg != "none" else ())
    return fig


def limit_pair(text):
    lo, _, hi = text.partition(",")
    return (as_number(lo) if lo.strip() else None, as_number(hi) if hi.strip() else None)


def key_value(text):
    column, _, value = text.partition("=")
    if not column or not _:
        raise argparse.ArgumentTypeError("expected COLUMN=VALUE")
    return (column, value)


def main():
    parser = argparse.ArgumentParser(
        description="Plot one column against another as a single-panel SVG.")
    parser.add_argument("--data", required=True, type=Path, help="CSV, JSON, or JSONL table")
    parser.add_argument("--x", required=True, help="column for the x axis")
    parser.add_argument("--y", required=True, action="append",
                        help="column for the y axis; repeat to draw one line per column")
    parser.add_argument("--series", help="column to split into one line each")
    parser.add_argument("--series-label", default="{}", help="legend text, with {} for the value")
    parser.add_argument("--filter", action="append", default=[], type=key_value,
                        metavar="COLUMN=VALUE", help="keep only matching rows; repeatable")
    parser.add_argument("--agg", choices=("median", "mean", "none"), default="median",
                        help="how to collapse repeats at one x (default: median)")
    parser.add_argument("--bin", type=int, metavar="N",
                        help="bucket a continuous x into N bands before aggregating "
                             "(log-spaced when --xscale log)")
    parser.add_argument("--points", action="store_true",
                        help="also draw the raw values as a faint cloud")
    parser.add_argument("--no-line", action="store_true",
                        help="markers only, for a categorical x or a scatter")
    parser.add_argument("--title", default="")
    parser.add_argument("--subtitle", default="")
    parser.add_argument("--note", default="", help="footnote line under the panel")
    parser.add_argument("--xlabel")
    parser.add_argument("--ylabel")
    parser.add_argument("--xscale", choices=("linear", "log"), default="linear")
    parser.add_argument("--yscale", choices=("linear", "log"), default="linear")
    parser.add_argument("--xlim", type=limit_pair, metavar="LO,HI")
    parser.add_argument("--ylim", type=limit_pair, metavar="LO,HI")
    parser.add_argument("--hline", type=float, help="draw a reference line at this y")
    parser.add_argument("--clamp", action="store_true",
                        help="pin out-of-range points to the axis instead of clipping them")
    parser.add_argument("--width", type=int, default=900)
    parser.add_argument("--height", type=int, default=520)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()

    if args.series and len(args.y) > 1:
        parser.error("--series and repeated --y both name the series; use one or the other")

    rows = read_table(args.data)
    if not rows:
        parser.error(f"{args.data} has no rows")
    fig = build(rows, args)
    if not any(s["points"] for s in fig.series):
        parser.error(f"no plottable rows: check --x/--y/--filter against {args.data}")
    print(fig.write(args.out))


if __name__ == "__main__":
    main()
