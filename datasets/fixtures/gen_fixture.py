#!/usr/bin/env python3
"""Deterministically generate the checked-in fixture dataset (mini.csv).

URL-shaped rows with realistic skew plus deliberate edge cases: empty
strings, single bytes, needle-overlap patterns (abab / aaaa), rows with
CSV-hostile characters (commas, quotes), and a few long rows. 200 rows so
multi-chunk runs (chunk_rows=64/128) exercise ragged tails.

Regenerating: python3 gen_fixture.py > mini.csv  (output is stable).
"""
import random

rng = random.Random(20260707)

hosts = ["google.com", "yandex.ru", "example.org", "jetbrains.com", "spiraldb.dev"]
paths = ["", "index.html", "search", "a/b/c.html", "img.png", "q.php"]
params = ["", "?q=rust", "?id=42&x=1", "?utm=abab", "#frag"]

rows = []
for _ in range(170):
    scheme = rng.choice(["http", "https", "https", "https"])
    row = f"{scheme}://{rng.choice(hosts)}/{rng.choice(paths)}{rng.choice(params)}"
    rows.append(row)

rows += [
    "",                                # empty row
    "",                                # another empty row
    "a",                               # single byte
    "abababab",                        # overlap patterns for multi_contains
    "aaaa",
    "aaa",
    'has,comma,and"quote"',            # CSV quoting
    "trailing/",
    "http",                            # equals a common prefix needle
    ".html",                           # equals a common suffix needle
    "x" * 500,                         # long row
    "http://google.com/" + "y" * 300 + ".html",
    "tab\tand newline\\n escapees",
    "mixed\x7f\x01bytes",
    "ENDS-with-google",
    "google",                          # exact needle as full row
]
while len(rows) < 200:
    rows.append(f"https://filler-{len(rows)}.net/pad")

rng.shuffle(rows)

def csv_field(s: str) -> str:
    if s == "":
        return '""'  # a bare empty line is not a record; a quoted one is
    if any(c in s for c in ',"\n\r'):
        return '"' + s.replace('"', '""') + '"'
    return s

print("data")
for r in rows:
    print(csv_field(r))
