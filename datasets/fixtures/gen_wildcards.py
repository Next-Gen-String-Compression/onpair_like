#!/usr/bin/env python3
"""Deterministically generate the wildcard fixture dataset (wildcards.csv).

Hand-chosen rows, not random ones: every row exists to separate two readings
of a LIKE pattern that a buggy matcher would conflate. Kept apart from
mini.csv on purpose — mini is bound to the blessed smoke suite and the gate
canary, so changing its bytes would invalidate truth that has nothing to do
with wildcards.

The groups, and what each one catches:

  ab?c        '_' as exactly one byte: zero (abc) and two (abXYc) must fail,
              and a row containing a literal '_' must match '%ab_c%' as a
              wildcard AND '%ab\\_c%' as a literal.
  prefix...   ordered multi-gap vs anchoring: '%prefix%suffix%' matches all
              three, 'prefix%suffix' only the ones that also end in suffix.
  metachar    rows holding a literal '%', '_' or '\\', so escape handling is
              observable rather than theoretical.
  multibyte   'Lóve' — 'ó' is two UTF-8 bytes, so '%L_ve%' must NOT match it
              and '%L__ve%' must. This pins the byte semantics.
  degenerate  empty row, one-byte rows, and repeats (abab, aaa) where greedy
              matching can go wrong.
  binary      a row with 0x00 and 0xff, because rows are byte strings.

Regenerating: python3 datasets/fixtures/gen_wildcards.py > wildcards.csv
(output is stable).
"""
import sys

ROWS = [
    # --- ab?c: the '_' group ---------------------------------------------
    "abc",                      # '_' cannot match zero bytes
    "abxc",
    "ab-c",
    "ab_c",                     # literal underscore in the data
    "abXYc",                    # '_' cannot match two bytes
    "xxabxczz",                 # same, unanchored
    "xxabczz",
    "xxab_czz",
    "xxabXYczz",
    # --- anchoring and ordered gaps --------------------------------------
    "prefix-middle-suffix",
    "prefixXXmiddleYYsuffix",
    "prefix-suffix",            # no middle: '%prefix%middle%suffix%' must fail
    "suffix-middle-prefix",     # right pieces, wrong order
    "xprefix-middle-suffixx",   # anchored patterns must fail on this
    # --- literal metacharacters in the data ------------------------------
    "100%",
    "100%off",
    "100_off",
    "a%b",
    "a_b",
    "a\\b",                     # a literal backslash
    "%",
    "_",
    "%%",
    "__",
    # --- multibyte: '_' is one BYTE --------------------------------------
    "Love",
    "L\xc3\xb3ve",              # 'ó' in UTF-8: 0xC3 0xB3, two bytes
    "L_ve",
    "Lxve",
    # --- degenerate and repeat-prone -------------------------------------
    "",
    "a",
    "ab",
    "abab",
    "aaa",
    "aXbXc",
    "abcdef",
    "abc-def",
    # --- byte strings, not text ------------------------------------------
    "a\x00c",
    "a\xffc",
    "\x00\xff",
]


def quote(field: str) -> str:
    """RFC 4180 minimal quoting, plus the empty field.

    Written by hand rather than with the csv module, which refuses to emit the
    NUL byte the binary rows need. The empty row must be written as `""`: a
    bare empty line is no record at all to a CSV reader, and the row would be
    silently dropped at ingest (it was, the first time)."""
    if field == "" or any(c in field for c in (",", '"', "\n", "\r")):
        return '"' + field.replace('"', '""') + '"'
    return field


def main() -> None:
    # Latin-1 so every row is written as the exact bytes listed above; the
    # non-ASCII rows spell their own UTF-8 encoding explicitly.
    out = sys.stdout.buffer
    out.write(b"data\n")
    for row in ROWS:
        out.write(quote(row).encode("latin-1") + b"\n")


if __name__ == "__main__":
    main()
