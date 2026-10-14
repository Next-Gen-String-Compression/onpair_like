# /// script
# requires-python = ">=3.11"
# dependencies = ["pypdf>=4"]
# ///
"""Check that every figure prints at scale one, so 7 pt type is 7 pt on paper.

    uv run paper/check_panels.py [--assets DIR] [--sections DIR]

Reads each `\\includegraphics[width=...]{figure/panel}` out of the sources,
works out the width that slot prints at from the enclosing `subfigure` or
`minipage` (or the
figure environment), and compares it with the PDF's own MediaBox. A scale
other than 1.00 means that figure's labels come out larger or smaller than the
rest, which is the one thing a reader notices across figures.
"""

import argparse
import re
from pathlib import Path

from pypdf import PdfReader

HERE = Path(__file__).resolve().parent
PREPRINT = HERE.parent.parent / "substring_search_preprint"
PT_IN = 1 / 72.27
TEXT_IN = 506.295 * PT_IN
COLUMN_IN = 241.14749 * PT_IN

INCLUDE = re.compile(r"\\includegraphics(?:\[(?P<opts>[^\]]*)\])?\{(?P<name>[^}]+)\}")
SUBFIG = re.compile(
    r"\\begin\{(?P<env>subfigure|minipage)\}\[[^\]]*\]\{(?P<frac>[0-9.]+)\\(?P<unit>textwidth|columnwidth)\}"
)
FIGENV = re.compile(r"\\(?P<kind>begin|end)\{figure(?P<star>\*?)\}")


def in_starred(text, pos):
    """Whether `pos` sits inside a `figure*` rather than a `figure`: the last
    environment opened before it and not yet closed. Asking only whether a
    starred one appears earlier in the file marks every include after the
    first `figure*` as full width."""
    stack = []
    for m in FIGENV.finditer(text, 0, pos):
        if m.group("kind") == "begin":
            stack.append(bool(m.group("star")))
        elif stack:
            stack.pop()
    return bool(stack) and stack[-1]


def slot_inches(text, pos, opts):
    """What one includegraphics at `pos` prints at, in inches."""
    sub = None
    for m in SUBFIG.finditer(text, 0, pos):
        sub = m  # the last subfigure or minipage opened before this include
    if sub and text.count("\\end{" + sub.group("env") + "}", sub.end(), pos) == 0:
        base = TEXT_IN if sub.group("unit") == "textwidth" else COLUMN_IN
        slot = float(sub.group("frac")) * base
    else:
        slot = TEXT_IN if in_starred(text, pos) else COLUMN_IN
    if opts is None:
        return slot  # natural size: the slot is what it has to fit in
    m = re.search(r"width=([0-9.]*)\\(linewidth|textwidth|columnwidth)", opts)
    if not m:
        return None  # a height= or scale= include: not checkable this way
    factor = float(m.group(1) or 1.0)
    base = {"linewidth": slot, "textwidth": TEXT_IN, "columnwidth": COLUMN_IN}[m.group(2)]
    return factor * base


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--assets", default=PREPRINT / "assets")
    ap.add_argument("--sections", default=PREPRINT / "sections")
    args = ap.parse_args()

    bad = 0
    for tex in sorted(Path(args.sections).glob("*.tex")):
        text = tex.read_text()
        for m in INCLUDE.finditer(text):
            pdf = Path(args.assets) / f"{m.group('name')}.pdf"
            if not pdf.exists():
                print(f"MISSING  {tex.name}: {m.group('name')}.pdf")
                bad += 1
                continue
            printed = slot_inches(text, m.start(), m.group("opts"))
            if printed is None:
                print(f"SKIP     {tex.name}: {m.group('name')} (no width= option)")
                continue
            box = PdfReader(pdf).pages[0].mediabox
            drawn = float(box.width) / 72  # PDF points are 1/72 in
            if m.group("opts") is None:
                # Printed as drawn, so the only question is whether it fits.
                # TeX overfulls on a hair, so there is no tolerance here.
                flag = "ok  " if drawn <= printed else "WIDE "
                if flag != "ok  ":
                    bad += 1
                print(f"{flag}     {m.group('name'):28} drawn {drawn:5.2f}in  slot    {printed:5.2f}in  natural")
                continue
            scale = printed / drawn
            flag = "ok  " if abs(scale - 1) <= 0.02 else "SCALE"
            if flag != "ok  ":
                bad += 1
            print(f"{flag}     {m.group('name'):28} drawn {drawn:5.2f}in  printed {printed:5.2f}in  x{scale:.2f}")
    raise SystemExit(1 if bad else 0)


if __name__ == "__main__":
    main()
