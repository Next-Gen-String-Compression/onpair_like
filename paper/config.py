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
# Share-of-rows axes labelled 0.01%, 1%, 100% instead of 10^-4, 10^-2, 1.
PERCENT_TICKS = True
# A fixed y top for a figure's first n panels, (n, top); the rest keep the
# fitted range.
Y_TOP = {"micro-datasets": (2, 200), "e2e-micro-datasets": (2, 25),
         "no-sort": (1, 220), "uniform-freq": (1, 220), "micro-rows": (2, 110),
         "micro-trends": (2, 200)}
# The figures that stay log under LINEAR_Y.
LOG_FIGURES = {"false-positives", "micro-arch", "micro-rows", "micro-trends"}
# micro-datasets marks where OnPair16's SIMD-cut becomes slower than
# uncompressed memmem.
CROSSING_LINE = False
# phase-shares draws the summed time of all stages as a black line on top.
PHASE_TOTAL_LINE = False
# phase-shares stacks the stages as areas, the top edge their total; on a
# linear axis only, a log axis keeps the lines.
PHASE_STACKED = True

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
# Versions are in the paper text, not the labels.
ENGINE = {"duckdb": "DuckDB", "vortex": "Vortex"}


def name(scheme, method):
    return f"{SCHEME[scheme]}, {METHOD[method]}"


# The colour of every series that stands for a system, in every figure. An
# engine's series take its scheme's: DuckDB with FSST searches by
# decompressing, its cut scan the FSST cut; Vortex 0.86.1 decompresses, Vortex
# with OnPair the OnPair16 cut.
COLOR = {
    "uncompressed": "#C7C7C7",
    "lz4-decode": "#2CA02C",
    "fsst-decode": "#E69F00",
    "onpair-decode": "#56B4E9",
    "fsst-automaton": "#D62728",
    "fsst-compiled": "#8C564B",
    "fsst-cut": "#D55E00",
    "onpair16-cut": "#0072B2",
    "onpair12-cut": "#17BECF",
}

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
    # ablation/: specs/ablation/<size>.toml, gen-ablation.sh. plot_no_sort.py's no-sort and
    # uniform-freq figures: onpair_spiral_debug and fsst_prefilter_debug with the symbol
    # table sorted or not and planned with counts or uniform frequencies. NEON only.
    "ablation": {"run": "ablation-medium-neon-2026-10-16"},
    # prefix-suffix/: specs/prefix-suffix/<size>.toml, gen-prefix-suffix.sh.
    # plot_prefix_suffix.py's micro-prefix-suffix figure. NEON only.
    "prefix-suffix": {"run": "prefix-suffix-fast-neon-2026-10-12"},
    # rows/: specs/rows/*.toml, gen-rows.sh. plot_micro.py's micro-rows figure,
    # the row group sweep beside the pre-selection (plot_selection.py's panel).
    "rows": {
        "rowgroup": {"run": "rowgroup-fast-avx512-2026-10-14b"},
        # specs/rows/rowgroup-neon.toml, gen-rows.sh on arm64.
        "rowgroup-neon": {"run": "rowgroup-fast-neon-2026-10-17"},
        "rowgroup-llvm": {"run": "rowgroup-llvm-fast-avx512-2026-10-14"},
        # Uniform rows and runs of 1-16 and 1-256 rows at each share.
        "selection": {"run": "selection-fast-avx512-2026-10-15b"},
    },
    # fig1/: specs/fig1/*.toml, gen-fig1.sh, NEON. plot_fig1.py. `selection` holds
    # FSST decompress and automaton.
    "fig1": {
        "selection": "micro-selection-1mi-fast-whole-neon-2026-10-12",
        "lz4": "fig1-lz4-neon-2026-10-14e",
        "uncompressed": "fig1-uncompressed-neon-2026-10-14e",
        "onpair": "fig1-onpair-neon-2026-10-14f",
        "fsst": "fig1-fsst-neon-2026-10-14f",
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
            "data/matcher_arm64_2026-10-14.csv",
            "data/matcher_x86_64_2026-10-14b.csv",
            "data/matcher_x86_64_avx512_2026-10-14b.csv",
            "data/matcher_fsst_arm64_2026-10-14.csv",
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
    "e2e": {"run": "e2e-neon-2026-10-14"},
    # e2e/micro.py, the micro spec's needles end to end. plot_e2e_micro.py.
    "e2e-micro": {"run": "e2e-micro-fast-neon-2026-10-18",
                  "uncompressed": "e2e-micro-fast-neon-2026-10-18-uncompressed"},
}
