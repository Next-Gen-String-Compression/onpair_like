"""Settings of the paper figures.

INPUTS names every figure's inputs, and is the only place they are named. Each
plot script reads its section by default; `--run`, `--data` or `--log` on the
command line override it for a one-off drawing.

    uv run paper/check_inputs.py            # missing, stale or mixed inputs
    uv run paper/check_inputs.py --update   # move each entry to its newest run
    paper/make-figures.sh                   # check, then redraw every figure

Run names are the directory names under results/, loaded into
results/bench.duckdb; paths are relative to paper/ or, for plan logs, to the
repository root.
"""

# Every figure's log y axis drawn linear from 0 instead.
LINEAR_Y = True
# The figures that stay log under LINEAR_Y.
LOG_FIGURES = {"false-positives", "phase-shares", "micro-arch", "micro-rows"}
# micro-datasets marks where OnPair16's SIMD-cut becomes slower than
# uncompressed memmem.
CROSSING_LINE = False

# The names every figure prints. A series is "<scheme>, <method>", an engine
# with our scan "<engine>, <scheme> <method>"; the captions and text are in the
# preprint and are not covered.
SCHEME = {
    "onpair": "OnPair",
    "onpair12": "OnPair12",
    "onpair16": "OnPair16",
    "fsst": "FSST",
    "lz4": "LZ4",
    "uncompressed": "Uncompressed",
}
METHOD = {
    "cut": "SIMD-cut",
    "automaton": "automaton",
    "compiled": "compiled automaton",
    "decode": r"decompress \& search",
    "memmem": "search",
}
ENGINE = {"duckdb": "DuckDB", "vortex": "Vortex", "vortex-release": "Vortex 0.86.1"}


def name(scheme, method):
    return f"{SCHEME[scheme]}, {METHOD[method]}"

INPUTS = {
    # micro/: specs/micro/<size>.toml, gen-micro.sh. plot_micro.py, one run per
    # machine in panel order: micro-arch, micro-datasets, micro-matrix,
    # false-positives. NEON and AVX-512; AVX2 only in kernel/. `bandwidth_line`
    # draws one core's memory read rate across micro-datasets and micro-arch.
    "micro": {
        "runs": {
            "ARM, NEON": "micro-1mi-medium-neon-2026-10-08",
            "x86, AVX-512": "micro-medium-avx512-2026-10-14b",
        },
        "bandwidth_line": True,
    },
    # prefix-suffix/: specs/prefix-suffix/<size>.toml, gen-prefix-suffix.sh.
    # plot_prefix_suffix.py's micro-prefix-suffix figure. NEON only.
    "prefix-suffix": {"run": "prefix-suffix-fast-neon-2026-10-12"},
    # rows/: specs/rows/*.toml, gen-rows.sh. plot_micro.py's micro-rows figure,
    # the row group sweep beside the pre-selection (plot_selection.py's panel).
    "rows": {
        "rowgroup": {"run": "rowgroup-fast-avx512-2026-10-14b"},
        "rowgroup-llvm": {"run": "rowgroup-llvm-fast-avx512-2026-10-14"},
        # Uncompressed memmem (row-hay) from its own run on the same box; the
        # selection run holds the replaced hay-sel.
        "selection": {"run": "selection-fast-avx512-2026-10-14b",
                      "uncompressed": "selection-uncompressed-avx512-2026-10-13"},
    },
    # fig1/: specs/fig1/{lz4,uncompressed,onpair}.toml, gen-fig1.sh.
    # plot_fig1.py; FSST decompress and automaton from the NEON selection run.
    "fig1": {
        "selection": "micro-selection-1mi-fast-whole-neon-2026-10-12",
        "lz4": "fig1-lz4-neon-2026-10-13",
        "uncompressed": "fig1-uncompressed-neon-2026-10-13",
        "onpair": "fig1-onpair-neon-2026-10-14",
        # Built from the FSST submodule's uncommitted per-row port
        # (FSST_SOURCE_DIR), not yet the pin in pins.env.
        "fsst": "fig1-fsst-neon-2026-10-14d",
    },
    # phases/: specs/phases/verify-<size>.toml, gen-phases.sh.
    # plot_phase_shares.py: one panel per series and machine, in panel order,
    # the series by its harness label. `split` is the split bench's matcher and
    # resolver timing of the run's needles, which divides the scan band; a
    # panel without one draws the scan undivided. The FSST port has no split yet.
    "phases": {
        "OnPair12, ARM, NEON": {
            "run": "verify-1mi-medium-neon-2026-10-08",
            "label": "onpair_spiral/pf_walk@bits=12,prune=1.0",
            "split": "data/verify_split_1mi_medium_m4pro_2026-10-09.csv",
        },
        "OnPair16, ARM, NEON": {
            "run": "verify-1mi-medium-neon-2026-10-08",
            "label": "onpair_spiral/pf_walk@bits=16,prune=1.0",
            "split": "data/verify_split_1mi_medium_b16_m4pro_2026-10-09.csv",
        },
        "FSST, ARM, NEON": {
            "run": "micro-1mi-medium-neon-2026-10-08",
            "label": "fsst_prefilter/fsst-prefilter",
        },
        "OnPair12, x86, AVX-512": {
            "run": "verify-medium-avx512-2026-10-14b",
            "label": "onpair_spiral/pf_walk@bits=12,prune=1.0",
            "split": "data/verify_split_1mi_medium_c7a_avx512_2026-10-08.csv",
        },
        "OnPair16, x86, AVX-512": {
            "run": "verify-medium-avx512-2026-10-14b",
            "label": "onpair_spiral/pf_walk@bits=16,prune=1.0",
            "split": "data/verify_split_1mi_medium_b16_c7a_avx512_2026-10-08.csv",
        },
        "FSST, x86, AVX-512": {
            "run": "micro-medium-avx512-2026-10-14b",
            "label": "fsst_prefilter/fsst-prefilter",
        },
    },
    # kernel/: gen-kernel.sh, gen-kernel-fsst.sh. plot_matcher.py: sweeps pinned in paper/data.
    # `bandwidth_line` draws one core's read rate at the sweep's 1 MiB in every panel.
    "kernel": {
        "bandwidth_line": True,
        "matcher": [
            "data/matcher_m4pro_2026-10-14.csv",
            "data/matcher_x86_64_2026-10-14b.csv",
            "data/matcher_x86_64_avx512_2026-10-14b.csv",
            "data/matcher_fsst_m4pro_2026-10-14.csv",
            "data/matcher_fsst_x86_64_2026-10-14b.csv",
            "data/matcher_fsst_x86_64_avx512_2026-10-14b.csv",
        ],
    },
    # bandwidth/run.sh: one core's sequential read rate per working-set size, the
    # CSV of the machine each instruction set ran on, for the `bandwidth_line`s.
    "bandwidth": {
        "NEON": "bandwidth/results/m4pro.csv",
        "AVX2": "bandwidth/results/c7a.xlarge.csv",
        "AVX-512": "bandwidth/results/c7a.xlarge.csv",
    },
    # plan-cut/: gen-plan-cut.sh over a micro spec. plot_plan_cut.py and
    # plot_kernel_choice.py.
    "plan-cut": {"log": "results/plan-log/micro-1mi-medium-2026-10-09"},
    # e2e/: e2e/run.py. plot_e2e.py: an e2e/run.py run, read from results/e2e/<run>.
    "e2e": {"run": "e2e-neon-2026-10-13d"},
    # e2e/micro.py, the micro spec's needles end to end. plot_e2e_micro.py.
    "e2e-micro": {"run": "e2e-micro-fast-neon-2026-10-13"},
}
