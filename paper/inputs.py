# /// script
# requires-python = ">=3.11"
# dependencies = ["duckdb>=1.1"]
# ///
"""The inputs of every figure from the runs in this tree, for make-figures.sh:
config.INPUTS with each run, sweep and log replaced by the newest one this tree
holds under the name its script writes, of the needle size the paper draws
from when there is one. An entry no run of this tree fills is null, and the
figures that read it are not drawn.

    uv run paper/inputs.py OUT.json

Writes OUT.json, which paperfig.inputs and check_inputs.py read when
PAPER_INPUTS names it, reports every entry on stderr, and prints on stdout the
scripts whose inputs are all present, one per line.
"""

import json
import re
import runpy
import sys
from pathlib import Path

import duckdb

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
DB = ROOT / "results" / "bench.duckdb"
DATE = r"\d{4}-\d{2}-\d{2}[a-z]?"
# The needle size the paper draws each block's figures from; a run of another
# size stands in only when this tree has none of this one (a --small run).
PAPER_SIZE = {"micro": "medium", "verify": "medium", "ablation": "medium", "plan-cut": "medium",
              "prefix-suffix": "fast", "e2e-micro": "fast"}
ARCH = {"ARM, NEON": "neon", "x86, AVX-512": "avx512"}
# The scripts make-figures.sh runs, in its order, each with the entries it reads.
SCRIPTS = {
    "plot_fig1": lambda i: list(i["fig1"].values()),
    "plot_matcher": lambda i: [i["kernel"]["matcher"] or None],
    "plot_phase_shares": lambda i: [p["run"] for p in i["phases"].values()],
    "plot_micro": lambda i: [*i["micro"]["runs"].values(), *(r["run"] for r in i["rows"].values())],
    "plot_prefix_suffix": lambda i: [i["prefix-suffix"]["run"]],
    "plot_selection": lambda i: [i["rows"]["selection"]["run"]],
    "plot_plan_cut": lambda i: [i["plan-cut"]["log"]],
    "plot_kernel_choice": lambda i: [i["plan-cut"]["log"]],
    "plot_e2e": lambda i: [i["e2e"]["run"]],
    "plot_e2e_micro": lambda i: [i["e2e-micro"]["run"]],
    "plot_no_sort": lambda i: [i["ablation"]["run"]],
    "table1": lambda i: [i["rows"]["rowgroup"]["run"] or i["rows"]["rowgroup-neon"]["run"]],
}


def date_of(name):
    m = re.search(rf"{DATE}$", name)
    return m.group(0) if m else ""


def newest(names, pattern, size=None):
    """The newest of `names` ({name: sort key}) matching `pattern`, which may
    hold one `(?P<size>...)` group; of `size` when one matches it."""
    hits = [(m.groupdict().get("size"), key, name) for name, key in names.items()
            if (m := re.fullmatch(pattern, name))]
    sized = [h for h in hits if h[0] == size]
    pool = sized or hits
    return max(pool, key=lambda h: h[1])[2] if pool else None


def sized(block, arch=None):
    tail = f"-{arch}" if arch else ""
    return rf"{re.escape(block)}-(?P<size>fast|medium|full){tail}-{DATE}"


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    inputs = runpy.run_path(str(HERE / "config.py"))["INPUTS"]
    runs = {}
    if DB.exists():
        con = duckdb.connect(str(DB), read_only=True)
        runs = dict(con.sql("select run_name, strftime(started_at, '%Y-%m-%d %H:%M:%S') "
                            "from run where is_latest").fetchall())
    # A run or log folder counts once it holds what the plots read: an
    # interrupted one has none of it.
    folders = {}
    for sub, needs in (("e2e", ("queries.csv", "needles.csv")), ("plan-log", ("queries.csv",))):
        d = ROOT / "results" / sub
        folders[sub] = {p.name: (date_of(p.name), p.stat().st_mtime) for p in d.iterdir()
                        if any((p / f).exists() for f in needs)} if d.is_dir() else {}

    report = []

    def found(what, value, how):
        report.append(f"{'found' if value else 'none ':6} {what}: {value or how}")
        return value

    for machine, arch in ARCH.items():
        inputs["micro"]["runs"][machine] = found(f"micro {machine}", newest(
            runs, sized("micro", arch), PAPER_SIZE["micro"]), f"gen-micro.sh on {arch}")
    inputs["ablation"]["run"] = found("ablation", newest(
        runs, sized("ablation", "neon"), PAPER_SIZE["ablation"]), "gen-ablation.sh on neon")
    inputs["prefix-suffix"]["run"] = found("prefix-suffix", newest(
        runs, sized("prefix-suffix", "neon"), PAPER_SIZE["prefix-suffix"]),
        "reproduce-neon.sh --prefix-suffix")
    for spec, entry in inputs["rows"].items():
        name = {"rowgroup-neon": "rowgroup-fast-neon"}.get(spec, f"{spec}-fast-avx512")
        entry["run"] = found(f"rows {spec}", newest(runs, rf"{name}-{DATE}"), "gen-rows.sh")
    for spec in inputs["fig1"]:
        inputs["fig1"][spec] = found(f"fig1 {spec}", newest(runs, rf"fig1-{spec}-neon-{DATE}"),
                                     "gen-fig1.sh on neon")
    for panel, entry in inputs["phases"].items():
        arch = next(a for m, a in ARCH.items() if panel.endswith(m))
        # The FSST panels read the micro run, which carries the same phases.
        entry["run"] = (inputs["micro"]["runs"][next(m for m in ARCH if panel.endswith(m))]
                        if panel.startswith("FSST") else found(f"phases {panel}", newest(
                            runs, sized("verify", arch), PAPER_SIZE["verify"]), f"gen-phases.sh on {arch}"))
    sweeps = {p.name: (date_of(p.stem), p.stat().st_mtime) for p in (HERE / "data").glob("matcher_*.csv")}
    matcher = []
    for rel in inputs["kernel"]["matcher"]:
        prefix = re.sub(rf"{DATE}\.csv$", "", Path(rel).name)
        best = newest(sweeps, rf"{re.escape(prefix)}{DATE}\.csv")
        found(f"matcher sweep {prefix}*", best and f"data/{best}" + (" (the shipped one)" if best == Path(rel).name
                                                                     else ""), "gen-kernel*.sh")
        if best:
            matcher.append(f"data/{best}")
    inputs["kernel"]["matcher"] = matcher
    log = newest(folders["plan-log"], sized("micro"), PAPER_SIZE["plan-cut"])
    inputs["plan-cut"]["log"] = found("plan log", log and f"results/plan-log/{log}", "gen-plan-cut.sh")
    inputs["e2e"]["run"] = found("e2e run", newest(folders["e2e"], rf"e2e-neon-{DATE}"), "e2e/run.py on neon")
    inputs["e2e-micro"]["run"] = found("e2e-micro run", newest(
        folders["e2e"], sized("e2e-micro", "neon"), PAPER_SIZE["e2e-micro"]), "e2e/micro.py on neon")
    # e2e/micro.py runs the uncompressed baseline with the other systems.
    inputs["e2e-micro"]["uncompressed"] = None

    Path(sys.argv[1]).write_text(json.dumps(inputs, indent=2) + "\n")
    print("\n".join(report), file=sys.stderr)
    for script, needs in SCRIPTS.items():
        if all(needs(inputs)):
            print(script)


if __name__ == "__main__":
    main()
