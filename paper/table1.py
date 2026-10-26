# /// script
# requires-python = ">=3.11"
# dependencies = ["matplotlib==3.10.6", "pandas>=2", "duckdb>=1.1", "numpy"]
# ///
"""Table 1, bytes per row and compression ratio per column at 128k and 512k
rows per row group, from a `paper/specs/rows/rowgroup*.toml` run, which builds
every column at both: FSST, OnPair12 and OnPair16 (simd_cut_onpair at 12 and
16 bits), each the uncompressed view's bytes over the candidate's footprint.

    uv run paper/table1.py [--run RUN] [--out DIR]

The run defaults to config.INPUTS["rows"]["rowgroup"], else ["rowgroup-neon"];
the footprint does not depend on the machine. Prints the table, and with
`--out` writes it to `DIR/table1.txt` too.
"""

import argparse
import json
from pathlib import Path

import paperfig as pf
import plot_micro as pm

COLUMNS = [
    ("imdb-charname-1mi", "Character names"),
    ("amazon-title-1mi", "Amazon book titles"),
    ("clickbench-url-1mi", "ClickBench URL"),
    ("dbpedia-abstract-1mi", "DBpedia abstracts"),
]
ROW_GROUPS = [(131072, "128k rows"), (524288, "512k rows")]
SCHEMES = ["FSST", "OP12", "OP16"]


def scheme(candidate, config):
    if candidate == "fsst":
        return "FSST"
    return f"OP{json.loads(config)['bits']}"


def load(run):
    """({dataset: bytes per row}, {(dataset, chunk_rows, scheme): ratio})."""
    con = pm.duckdb.connect(str(pm.DB), read_only=True)
    rows = con.sql(
        """
        select dataset, candidate, config, chunk_rows,
               any_value(mean_len), any_value(compression_ratio)
        from v where run_name = ? and is_latest and status = 'ok'
          and candidate in ('fsst', 'simd_cut_onpair') and chunk_rows in (131072, 524288)
        group by all
        """,
        params=[run],
    ).fetchall()
    per_row = {d: mean for d, _, _, _, mean, _ in rows}
    ratio = {(d, n, scheme(c, cfg)): r for d, c, cfg, n, _, r in rows}
    return per_row, ratio


def table(per_row, ratio):
    cell = lambda v, w: f"{v:{w}.2f}" if v is not None else f"{'-':>{w}}"
    head = "  ".join(f"{label:<17}" for _, label in ROW_GROUPS)
    lines = [f"{'':<19}{'':>6}  {head}".rstrip(),
             f"{'column':<19}{'B/row':>6}  " + "  ".join(" ".join(f"{s:>5}" for s in SCHEMES)
                                                      for _ in ROW_GROUPS)]
    for d, label in COLUMNS:
        b = f"{per_row[d]:6.0f}" if d in per_row else f"{'-':>6}"
        lines.append(f"{label:<19}{b}  " + "  ".join(
            " ".join(cell(ratio.get((d, n, s)), 5) for s in SCHEMES) for n, _ in ROW_GROUPS))
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", default=None)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    rows = pf.inputs("rows")
    run = args.run or rows["rowgroup"]["run"] or rows["rowgroup-neon"]["run"]
    per_row, ratio = load(run)
    if not ratio:
        raise SystemExit(f"no build rows in {run}; run paper/gen-rows.sh first")
    text = table(per_row, ratio)
    print(f"Table 1 from {run}\n\n{text}")
    if args.out:
        Path(args.out).mkdir(parents=True, exist_ok=True)
        (Path(args.out) / "table1.txt").write_text(text + "\n")
        print(f"-> {Path(args.out) / 'table1.txt'}")


if __name__ == "__main__":
    main()
