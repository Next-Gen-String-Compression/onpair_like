# paper

Data and figures for the kernel section of the substring-search preprint
(`../../substring_search_preprint`). Nothing here is built by the workspace.

The figures fall into blocks. Each block has its specs under `specs/<block>/`,
one `gen-<block>.sh` that runs them, and one section of `INPUTS` in `config.py`.

| block | specs | machines | row groups | run by | figures |
| --- | --- | --- | --- | --- | --- |
| `micro` | `specs/micro/{fast,medium,full}.toml` | NEON, AVX-512 | 512k | `gen-micro.sh <size> [neon\|avx512]` -> `micro-<size>-<arch>-<date>` | `micro-arch`, `micro-datasets`, `micro-matrix`, `false-positives` |
| `prefix-suffix` | `specs/prefix-suffix/{fast,medium,full}.toml` | NEON | 512k | `gen-prefix-suffix.sh <size>` -> `prefix-suffix-<size>-neon-<date>`, generating missing suites first | `micro-prefix-suffix` |
| `rows` | `specs/rows/{rowgroup,rowgroup-llvm,selection}.toml`, `rowgroup-neon.toml` | AVX-512; `rowgroup-neon` on NEON | whole, 512k .. 2k (LLVM to 32k); selection: whole, 2^-9 .. 2^-1 | `gen-rows.sh` -> `{rowgroup,rowgroup-llvm,selection}-fast-avx512-<date>` on x86, `rowgroup-fast-neon-<date>` on arm64 | `micro-rows` |
| `phases` | `specs/phases/verify-<size>.toml` | NEON, AVX-512 | 512k | `gen-phases.sh <size> [neon\|avx512]` -> `verify-<size>-<arch>-<date>` | `phase-shares` |
| `kernel` | - | NEON, AVX2, AVX-512 | - | `gen-kernel.sh`, `gen-kernel-fsst.sh` -> `data/matcher_*.csv` | `kernel-matcher` |
| `plan-cut` | a micro spec | - | 512k | `gen-plan-cut.sh <size>` -> `results/plan-log/micro-<size>-<date>/` | `plan-cut`, `kernel-choice` |
| `ablation` | `specs/ablation/{fast,medium}.toml` | NEON | 512k | `gen-ablation.sh <size>` -> `ablation-<size>-neon-<date>`; the `simd_cut_onpair_debug` and `simd_cut_fsst_debug` candidates with `"sorted": false` or `"uniform": true` | `no-sort`, `uniform-freq` |
| `e2e` | - | NEON | engine default | `e2e/run.py` -> `results/e2e/<run>`, JOB; with `--workloads clickbench,job` (and `E2E_CLICKBENCH=1 e2e/data.sh`) ClickBench too | `e2e-job-duckdb` (also `e2e-*-speedup`, `e2e-queries`, `e2e-scans`, not in the paper; their ClickBench panels only from a run with ClickBench) |
| `e2e-micro` | `specs/micro/<size>.toml`'s suites | NEON | engine default | `e2e/micro.py [--spec <size>]` -> `results/e2e/e2e-micro-<size>-neon-<date>` | `e2e-micro-datasets`, `e2e-micro-pooled` |

`llvm-simd`, the JIT-compiled pattern automaton, is in the micro and rows
specs; it registers only on an x86 build against LLVM 14-16. Check the inputs
with `uv run paper/check_inputs.py [--update]`, and redraw every figure with
`paper/make-figures.sh [--out DIR]`.

`INPUTS` in `paper/config.py` names every figure's inputs: the runs, sweeps and plan log
it is drawn from. The plot scripts read it unless given `--run`, `--data` or
`--log`, and write to the preprint's `assets/` unless given `--out`.
`check_inputs.py` reports an entry that is missing, one with a newer run or
file of the same name, figures drawn from different needle sizes, micro runs
without a series `plot_micro.py` draws, and a phase-shares split that does not
cover its run; `--update` moves stale entries to the newest, which
every `gen-*.sh` does after its runs. Changing needle size or machine is an
edit to `config.py`. `make-figures.sh` runs the check and stops on any
finding before it draws. A multi-panel figure writes **one PDF
per panel into its own folder**, `assets/<figure>/1.pdf`, `2.pdf`, ... plus
`legend.pdf` where the series need one, so `assets/` lists figures rather than
dozens of panels; a single-panel figure stays a file. The grid is assembled in
the section source with `subfigure`, so a figure can be re-arranged without
re-running anything.

Type size is the same in every figure, and that rests on two rules:

* `paperfig.style()` is the only place that sets sizes, `BODY_PT` and
  `SMALL_PT`. No script passes a numeric `fontsize`.
* A panel is drawn at the width it prints at (`PANEL_3UP`, `PANEL_2UP`,
  `COLUMN_IN`, `TEXT_IN`, measured from the template's `\textwidth` of
  506.295 pt and `\columnwidth` of 241.147 pt) and included with
  `width=\linewidth`, so LaTeX scales it by one. Including with `height=` or
  a different fraction than the panel was drawn for silently resizes the type.

Label text goes through LaTeX (`paperfig.TEX_PREAMBLE`) with the three font
packages acmart loads, libertine, zi4 and newtxmath, so a label is set in the
same face as the body text beside it. The scripts therefore need a TeX install
with those packages on the path, and a label with a `%`, `_`, `&` or a
non-ASCII character has to be written as LaTeX.

`uv run paper/check_panels.py` enforces the second rule: it reads every
`\includegraphics` in `sections/`, works out the width that slot prints at,
compares it with the PDF's MediaBox and exits non-zero on any scale off 1.00. `plot_matcher.py` draws one
column of panels per instruction set, read off each CSV's `machine` column;
an encoding with no rows for one of them gets the fitted cost model drawn
faint in its place.

`data/` holds one sweep per instruction set: NEON on the M4 Pro, AVX2 and
AVX-512 on the EPYC 9R14 of a `c7a.xlarge`, the matcher sweeps from
2026-10-14 (all three; the x86 ones named `_2026-10-14b`). The u8 sweeps of the FSST
port are named `*_fsst_*`. Every matcher sweep walks 1 MiB of code stream, so
the two encodings are read against one memory ceiling: 2^19 codes for u16
(`loader.rs`'s default, `SWEEP_CODES` overrides it) and `loader.hpp`'s
`CODES = 1u << 20` on the FSST side. At 8 MiB a vector kernel on a server
core measures the memory system rather than itself (see `bandwidth/`); 1 MiB
stays in L2 on every machine fitted.
**The AVX-512 kernels are compile-time gated** on `target_feature =
"avx512bw"`, so a default build reports `avx2` in the `machine` column; run
the sweep twice on an x86 host, once plainly and once under
`RUSTFLAGS="-C target-cpu=native"`, and name the second CSV
`matcher_x86_64_avx512_<date>.csv`.

The OnPair sweep lives in `scan::bench::matcher_fit::sweep`. The walk
has no sweep of its own; its cost comes out of the harness run, see below.

`reproduce-kernel.sh` does the whole kernel block from a clone: it runs both
gen scripts (twice on an AVX-512 host, native build first) and draws the
figure into `paper/out/kernel-<date>/`. Both build the kernels from this
repository, `third_party/onpair` and `third_party/fsst`.
The matcher sweeps read one stream, `ch/hits/URL_1m`, which the scripts build
when missing: `gen-kernel.sh` runs onpair's `bench_corpus ch` over a
`ch.duckdb` in `target/kernel-corpus/` (the `duckdb` CLI on `PATH`) and
`bench_needles` over the OnPair streams. It builds that `ch.duckdb` from the
first two ClickBench parquet files, which hold the corpus's 2^20 rows in table
order, from `datasets/raw/clickbench/`, where `datasets/prepare.py` downloads
them (300 MB) and `gen-kernel.sh` does when they are missing; the streams come
out byte-identical to the ones from `e2e/data.sh`'s full table. Then
`gen-kernel-fsst.sh` then encodes onpair's raw `URL_1m.csv` with
`prefilter_corpus` and draws its catalog with the same `bench_needles`. Run
`gen-kernel.sh` first. Besides the Rust and C++ toolchains the sweeps need nothing else;
the plot needs `uv` and a TeX install with libertine, zi4 and newtxmath. The
URL stream changed when `ch.duckdb` was rewritten on 2026-09-17, so a matcher
sweep run since reads different codes from the pinned ones.

The instruction counts in the section's table are read off the kernels in
`onpair/src/search/prefilter/scan/matcher/`, and the fitted ns per code from
`scan/policy/mod.rs`, which `cargo test --release --lib scan::bench::fit --
--ignored --nocapture` refits from a matcher CSV.

`plot_phase_shares.py` draws one panel per entry under `"phases"` in
`config.py`: a series by its harness label, the run it is read from and,
where one exists, its split CSV. Six panels: OnPair12, OnPair16 and the FSST
port (`simd_cut_fsst`) on NEON and on AVX-512. OnPair comes from a
`verify-*` run, `simd_cut_onpair`'s `simd-cut` strategy, the minimum cut
walked and rows exact; the FSST port and OnPair16 on AVX-512 come from the
`micro-*` run of the same revision, which carries the same instrumented
phases. The verify cost is one timed pass in the candidate's instrumented
pass, reported as `verify_ns`: the alignment walks over the hits
`prefilter_hits` located beforehand and untimed. Each phase is the fastest of
five runs after a warm-up, so a rare needle is still resolved rather than
lost in the clock's step. `phase-shares` stacks prep, matcher, resolver and
walk as shares of each query's summed time against selectivity, averaged
over a half-decade bin, over the four columns pooled. The paper includes
`onpair12-arm-neon.pdf` only and says the others look similar; the rest stay
in the assets. The scan is the harness's fused `prefilter_ns`, divided
between matcher and resolver in the proportion the split CSV gives them; a
panel without a split draws it as one band, with `legend-unsplit.pdf` for its
three entries.

The matcher and resolver bands do not come from that run. The two scan stages
are fused per block and the library exposes no split timing, so they are
measured by an out-of-tree bench that is copied into onpair for the run and
removed again; its output is pinned here as
`data/verify_split_1mi_medium_m4pro_2026-10-03.csv` (12 bits), `_b16_` (16
bits) and `_c7a_avx512_` (both, on the EPYC), keyed by `query_id`.
Its resolver is always the gallop seek, the one the paper describes, whatever
the planner would pick. Each covers the medium set's 2,053 needles, so
`config.py` pins it with a medium run; a run over another needle set needs
its own split, which `check_inputs.py` reports. Regenerate it when the
kernels or the needle set change. Timed as two passes it sums to within a
fifth of the fused scan rather than partitioning it exactly. The bench is
onpair's, so the FSST port has no split.

`paper/specs/micro/full.toml` runs our prefilter (12 and 16 bits, pruned)
against `simd_cut_onpair/decode+memmem-hay` (16 bits), `fsst/decode+memmem-hay`,
`uncompressed/direct+memmem-hay` and `fsst_like_tum/interp` at 512k-row
row groups, on four columns of 1,048,576 rows each: ClickBench URLs, Amazon book
titles, DBpedia abstracts and IMDb `char_name.name`, the last with nearly
every row distinct. `memmem-hay` searches a chunk's whole payload in one pass and maps each
hit back to its row, so all three baselines decode and scan a chunk at a time.
Its needles come from the SA + LCP generator of
`experiments/matcher_selection`, 20 per grid cell and 4,052 in total. Run it on
an idle machine: a load average above 2 slows the single-threaded baselines
several times and shows as a coefficient of variation above 0.05 in the
`stddev_ns / mean_ns` column. `micro/fast.toml` is the same four columns
and the same grid cells with one needle per cell, 211 in total and about half
an hour, which `paper/gen-micro.sh fast` runs. `medium` takes the
ten-per-cell suites, 2,053 needles and about four and a half hours. All three
cover identical cells, so the smaller ones are strict subsets rather than
different samples, and a figure drawn from one has the same shape with fewer
points per cell.

Measured on 2026-09-22, and the three sizes scale from it: a run costs two
minutes of dictionary builds whatever its size, and the rest is proportional to
the needle count. The fast set took 29 minutes on the M4 Pro and 34 on the EPYC
per build, which puts the medium set at 4.5 and 5.3 hours and the full one at
8.8 and 10.4.

`gen-micro.sh` names its runs `micro-<size>-<arch>-<date>`,
and `check_inputs.py --update` moves `config.py` onto them where it pins an
older run of the same size and instruction set. The panels follow the runs:
four columns are drawn at `PANEL_4UP`, 0.24 of the text width.

Instruction sets: the matcher sweeps cover NEON, AVX2 and AVX-512; the micro
and verify runs cover NEON on the M4 Pro and AVX-512 on the EPYC only.
`micro-arch/` is one panel per instruction set, the M4 Pro and the AVX-512
build on the x86 box (`target/` there). The `"micro"` keys of `config.py` name them, and `pf.slug` turns those
labels into the file names `6_evaluation.tex` includes - a label changed
without the `\includegraphics` line beside it leaves a stale panel in
`assets/`. The two decompress-first baselines are drawn flat and dashed at
their median (`BASELINES` there), without markers or a quartile band; plaintext
memmem is drawn as a curve like the compressed-domain systems.

`plot_matcher.py` draws both encodings into every panel of
`kernel-matcher/`: onpair16's u16 codes solid, the u8 sweep of the FSST port
(`third_party/fsst/search/prefilter/scan/bench`) dashed, one colour per kernel. The u16
build has no plain nibble bitmap and the u8 build no byte table, so each has a
series the other does not; the u8 sweep does measure a byte table, and
`DROPPED` keeps it out. The u8 stream is the same ClickBench URL prefix
encoded with FSST; the catalog comes from onpair's `bench_needles` over that
stream. Both grids now span the same axes: the |R| series is read at the widest
width whose |R| disjoint code ranges fit the code space, 16 codes for u16 and
8 for u8 (`RANGE_WIDTH`), and the u8 point at |C| = 256 is the whole 256-symbol
dictionary, named by the sweep because the catalog stops at 192. An encoding with no sweep for an instruction set is drawn faint
from the fitted cost model, and the panel says so.
