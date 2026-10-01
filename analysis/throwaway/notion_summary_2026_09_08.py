#!/usr/bin/env python3
"""Two figures for the Notion write-up of the 2026-09-08 prefilter results.

Run from the repo root:

    python analysis/throwaway/notion_summary_2026_09_08.py

Figure 1 is the URL-literals series v10..v21, one point per run, three
quantiles per dictionary width: the claim is that p90 never moved and p10
moved 8x. Figure 2 is the extended sweep, local working tree against the
pinned upstream rev, harmonic mean per corpus.

Both read results/bench.duckdb and write into figures/. The run ids and the
run-key parsing are pinned to that db, so this is not reusable past the day
it was written; analysis/plot.py is the part that is.
"""

import sys
from pathlib import Path

import duckdb

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from plot import PALETTE, Figure  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
DB = ROOT / "results" / "bench.duckdb"
OUT = ROOT / "figures"

B12 = "onpair_spiral/pf_memmem@bits=12"
B16 = "onpair_spiral/pf_memmem@bits=16"
P12 = "onpair_spiral_pinned/pf_memmem@bits=12"
P16 = "onpair_spiral_pinned/pf_memmem@bits=16"
PRUNED = {12: B12 + ",prune=1.0", 16: B16 + ",prune=1.0"}

# v15 ran on a binary built before the edits it was meant to measure, and was
# discarded the day it was taken. Leaving it in would draw a dip that is an
# artefact of the build, not of the code.
SKIP_RUNS = {"v15"}


def quantile_series(con):
    """Per run and per dictionary width: min, p25, median, p75, max.

    One entry per width, so the figure reads as two distributions moving rather
    than six unrelated lines.
    """
    runs = con.execute(
        """
        select run_id, run_key from run
        where run_key like '%url-literals-1m-v1%' or run_key like '%url-literals-1m-v2%'
        order by run_id
        """
    ).fetchall()
    name = {r: k.split("/")[1].split("@")[0].replace("url-literals-1m-", "") for r, k in runs}
    ids = ",".join(str(r) for r, _ in runs)
    rows = con.execute(
        f"""
        select run_id, label,
               min(gbps) as lo,
               quantile_cont(gbps, 0.25) as p25,
               median(gbps) as p50,
               quantile_cont(gbps, 0.75) as p75,
               max(gbps) as hi
        from v
        where run_id in ({ids}) and chunk_rows = 0 and status = 'ok'
          and gbps is not null and label in ('{B12}', '{B16}')
        group by 1, 2
        """
    ).fetchall()
    out = {}
    for run_id, label, lo, p25, p50, p75, hi in rows:
        tag = name[run_id]
        if tag in SKIP_RUNS:
            continue
        bits = 12 if label == B12 else 16
        out.setdefault(bits, []).append((tag, lo, p25, p50, p75, hi))
    for series in out.values():
        series.sort(key=lambda row: int(row[0][1:]))
    return out


def prune_ratio(con):
    """Pruned throughput over unpruned, per run and per (bits, chunking).

    A ratio, not a rate: 1.0 means PruneFactor bought nothing. The url-literals
    spec is the only one measured often enough to draw a line through.
    """
    runs = con.execute(
        """
        select run_id, run_key from run
        where run_key like '%url-literals-1m-v1%' or run_key like '%url-literals-1m-v2%'
        order by run_id
        """
    ).fetchall()
    name = {r: k.split("/")[1].split("@")[0].replace("url-literals-1m-", "") for r, k in runs}
    ids = ",".join(str(r) for r, _ in runs)
    labels = "','".join([B12, B16, PRUNED[12], PRUNED[16]])
    rows = con.execute(
        f"""
        select run_id, chunk_rows, label, count(*) / sum(1.0 / gbps) as hmean
        from v
        where run_id in ({ids}) and status = 'ok' and gbps is not null
          and label in ('{labels}')
        group by 1, 2, 3
        """
    ).fetchall()
    cells = {}
    for run_id, chunk, label, hmean in rows:
        cells.setdefault((name[run_id], chunk), {})[label] = hmean
    out = {}
    for (tag, chunk), got in cells.items():
        if tag in SKIP_RUNS:
            continue
        where = "whole column" if chunk == 0 else "chunked"
        for bits, plain in ((12, B12), (16, B16)):
            if plain in got and PRUNED[bits] in got:
                key = f"{bits} bits, {where}"
                out.setdefault(key, []).append((tag, got[PRUNED[bits]] / got[plain]))
    for points in out.values():
        points.sort(key=lambda p: int(p[0][1:]))
    return out


def walk_vs_verify(con):
    """GB/s against selectivity for the four verify paths, run 99.

    The walk's advantage is a function of how many rows survive the scan, so
    selectivity is the only sensible x axis here.
    """
    labels = {
        "onpair_spiral/pf_memmem@bits=16": "walk (compressed domain)",
        "onpair_spiral/pf_memmem_superset@bits=16": "decode + memmem",
        "onpair_spiral/pf_kmp_superset@bits=16": "token KMP",
        "onpair_spiral/kmp@bits=16": "no prefilter",
    }
    joined = "','".join(labels)
    rows = con.execute(
        f"""
        select label, selectivity, gbps
        from v
        where run_id = 99 and chunk_rows = 0 and status = 'ok'
          and label in ('{joined}') and gbps is not null
        order by selectivity
        """
    ).fetchall()
    out = {}
    for label, sel, gbps in rows:
        out.setdefault(labels[label], []).append((sel, gbps))
    return out


def local_vs_pinned(con, run_id):
    rows = con.execute(
        f"""
        select dataset, label, count(*) / sum(1.0 / gbps) as hmean
        from v
        where run_id = {run_id} and chunk_rows = 0 and status = 'ok'
          and gbps is not null
          and label in ('{B12}', '{B16}', '{P12}', '{P16}')
        group by 1, 2 order by 1
        """
    ).fetchall()
    pretty = {
        B12: "local, 12 bits", B16: "local, 16 bits",
        P12: "pinned, 12 bits", P16: "pinned, 16 bits",
    }
    short = {
        "clickbench-url-1m": "clickbench url",
        "dbpedia-abstract": "dbpedia",
        "msmarco-query": "msmarco",
        "tpch-ccomment-sf10": "tpch comment",
        "tpch-pname-sf10": "tpch pname",
    }
    out = {}
    for dataset, label, hmean in rows:
        out.setdefault(pretty[label], []).append((short.get(dataset, dataset), hmean))
    return out


def main():
    con = duckdb.connect(str(DB), read_only=True)

    latest = con.execute(
        "select run_id, run_key from run where run_key like '%literals-extended%' order by run_id desc limit 1"
    ).fetchone()

    OUT.mkdir(exist_ok=True)

    fig = Figure(
        title="Every gain landed in the tail",
        subtitle="onpair_spiral / pf_memmem, 8 URL literals over clickbench-url-1m, whole column",
        xlabel="benchmark run",
        ylabel="GB/s over payload bytes",
        yscale="log",
        note="Line is the median of the 8 needles, band is p25 to p75, dashed lines are "
             "min and max. v15 omitted: it ran on a stale binary and was discarded.",
        width=980,
        height=520,
    )
    for bits, series in sorted(quantile_series(con).items()):
        color = PALETTE[0]["color"] if bits == 12 else PALETTE[1]["color"]
        fig.add(
            f"{bits} bits",
            [(tag, p50) for tag, _, _, p50, _, _ in series],
            band=[(tag, p25, p75) for tag, _, p25, _, p75, _ in series],
            style={"color": color},
        )
        for index in (1, 5):  # min, max
            fig.add(
                "",
                [(row[0], row[index]) for row in series],
                style={"color": color, "dash": "6 4"},
                markers=False,
                legend=False,
            )
    fig.write(OUT / "tail-not-peak.svg")

    fig = Figure(
        title="Working tree against the pinned upstream rev",
        subtitle=f"pf_memmem, harmonic mean over 8 literals per corpus, whole column ({latest[1].split('/')[1].split('@')[0]})",
        xlabel="corpus",
        ylabel="GB/s over payload bytes",
        yscale="log",
        note="Pinned is spiraldb/onpair @ 5927cce, the revision the local checkout started from.",
        width=980,
        height=520,
    )
    for label, points in local_vs_pinned(con, latest[0]).items():
        # Markers only: the x axis is five unrelated corpora, and a line across
        # them would read as a trend that does not exist.
        fig.add(label, points, line=False)
    fig.write(OUT / "local-vs-pinned.svg")

    fig = Figure(
        title="Post-training pruning stopped paying",
        subtitle="pruned over unpruned throughput, pf_memmem, 8 URL literals over clickbench-url-1m",
        xlabel="benchmark run",
        ylabel="speedup from PruneFactor 1.0",
        note="Above 1.0 pruning is worth it. On this column it still costs 1.5% of compression "
             "ratio at 16 bits whole-column and 3.7% chunked, whether or not it buys throughput.",
        hline=1.0,
        width=980,
        height=460,
    )
    for label, points in prune_ratio(con).items():
        fig.add(label, points)
    fig.write(OUT / "prune-stopped-paying.svg")

    fig = Figure(
        title="What the alignment walk is worth",
        subtitle="pf_memmem at 16 bits, 8 URL literals over clickbench-url-1m, whole column",
        xlabel="selectivity (fraction of rows matching)",
        ylabel="GB/s over payload bytes",
        xscale="log",
        yscale="log",
        note="The walk verifies in the compressed domain, so its lead grows with the number of "
             "survivors. Below 1e-4 the verifier is free either way.",
        width=980,
        height=460,
    )
    for label, points in walk_vs_verify(con).items():
        fig.add(label, points)
    fig.write(OUT / "walk-vs-verify.svg")

    con.close()
    print(f"wrote {OUT / 'walk-vs-verify.svg'}")
    print(f"wrote {OUT / 'prune-stopped-paying.svg'}")
    print(f"wrote {OUT / 'tail-not-peak.svg'}")
    print(f"wrote {OUT / 'local-vs-pinned.svg'}")


if __name__ == "__main__":
    main()
