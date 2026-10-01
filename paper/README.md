# paper

Data and figures for the kernel section of the substring-search preprint
(`../../substring_search_preprint`). Nothing here is built by the workspace.

| step | command | output |
| --- | --- | --- |
| stage one, stage two sweeps | `paper/gen-kernel-data.sh` | `paper/data/{matcher,resolver}_<arch>_<date>.csv` |
| stage one and two sweeps of the FSST port | `paper/gen-kernel-data-fsst.sh` | `paper/data/{matcher,resolver}_fsst_<arch>_<date>.csv` |
| FSST resolver figure | `uv run paper/plot_resolver.py --data paper/data/resolver_fsst_<arch>_<date>.csv --name kernel-resolver-fsst` | `assets/kernel-resolver-fsst.pdf` in the preprint |
| minimum cut sizes, 2^20-row roster | `paper/gen-plan-log.sh [--fast\|--medium]` | `results/plan-log/<tag>-<date>/` |
| micro-benchmark and verifiers, 2^20-row roster | `paper/gen-micro-data.sh [--fast\|--medium] [neon\|avx512]` | runs `micro-1mi[-fast\|-medium]-<arch>-<date>` and `verify-1mi[-fast\|-medium]-<arch>-<date>` in `results/bench.duckdb` |
| check the inputs | `uv run paper/check_inputs.py [--update]` | findings against `paper/inputs.toml` |
| every figure | `paper/make-figures.sh [--out DIR]` | `assets/` in the preprint |

`paper/inputs.toml` names every figure's inputs: the runs, sweeps and plan log
it is drawn from. The plot scripts read it unless given `--run`, `--data` or
`--log`, and write to the preprint's `assets/` unless given `--out`.
`check_inputs.py` reports an entry that is missing, one with a newer run or
file of the same name, figures drawn from different needle sizes, micro runs
without a series `plot_micro.py` draws, and a verify split that does not cover
its run; `--update` moves stale entries to the newest, which
`gen-micro-data.sh` does after its runs. Changing needle size or machine is an
edit to `inputs.toml`. `make-figures.sh` runs the check and stops on any
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
2026-09-21 and the resolver sweeps from 2026-09-15. The u8 sweeps of the FSST
port are named `*_fsst_*`. Every matcher sweep walks 1 MiB of code stream, so
the two encodings are read against one memory ceiling: 2^19 codes for u16
(`loader.rs`'s default, `SWEEP_CODES` overrides it) and `loader.hpp`'s
`CODES = 1u << 20` on the FSST side. At 8 MiB a vector kernel on a server
core measures the memory system rather than itself (see `bandwidth/`); 1 MiB
stays in L2 on every machine fitted. The resolver sources now walk 8 MiB,
`CODES = 1 << 22` in `resolver_fit.rs` and `RESOLVER_CODES = 8u << 20` in
`resolver_fit.cpp`, the IMDb stream at its own shorter length. The pinned
resolver CSVs predate part of that: `resolver_c7a` and `resolver_fsst_x86_64`
walk 8 MiB, `resolver_fsst_m4pro` 1 MiB.
**The AVX-512 kernels are compile-time gated** on `target_feature =
"avx512bw"`, so a default build reports `avx2` in the `machine` column; run
the sweep twice on an x86 host, once plainly and once under
`RUSTFLAGS="-C target-cpu=native"`, and name the second CSV
`matcher_x86_64_avx512_<date>.csv`. Superseded sweeps move to
`data/superseded/`, outside the glob, rather than being deleted.

Two things the 2026-09-10 scan refactor changed here. The sweeps moved to
`scan::bench::{matcher_fit,resolver_fit}::sweep` (the gen script follows
them), and the walk sweep is gone with the standalone walk-cost bench, so
`walk_*.csv` cannot be regenerated — the walk's cost now comes out of the
harness run instead, see below. The batched nibble kernel was also renamed
`nibble_n_8k` -> `nibble_n8k`; `plot_matcher.py` folds the old spelling into
the new one so pre-refactor CSVs still draw.

**`resolver` cannot be swept on a fresh machine right now**: its `STREAMS`
wants `onpair/bench/data/imdb/name/name_1m.onpair16.csv`, which exists in no
checkout here. Regenerate it with the out-of-tree `bench_corpus` +
`bench_needles` examples against the IMDb name data before running that sweep.

The instruction counts in the section's table are read off the kernels in
`onpair/src/search/prefilter/scan/matcher/`, and the fitted ns per code from
`scan/policy/mod.rs`, which `cargo test --release --lib scan::bench::fit --
--ignored --nocapture` refits from a matcher CSV.

`plot_verify_selectivity.py` reads three `onpair_spiral` strategies from a
`verify-1mi*` run: `pf_walk` (the minimum cut walked, rows exact),
`pf_memmem_superset` and `pf_kmp_superset` (the same minimum cut with the walk
off, each verifier running). Each verify cost is one timed pass in the
candidate's instrumented pass, reported as `verify_ns`: for `pf_walk` the
alignment walks over the hits `prefilter_hits` located beforehand and untimed,
for the other two their own verifier with the token automaton's table build
charged to `setup_ns`. Each phase is the fastest of five runs after a warm-up,
so a rare needle is still resolved rather than lost in the clock's step.
`verify-selectivity` draws all three per row of the column against
selectivity, in half-decade bins, over the four columns pooled.

Its matcher and resolver lines do not come from that run. The two scan stages
are fused per block and the library exposes no split timing, so they are
measured by an out-of-tree bench that is copied into onpair for the run and
removed again; its output is pinned here as
`data/verify_split_1mi_medium_m4pro_2026-09-23.csv`, keyed by `query_id`.
It covers the medium set's 2,155 needles at 12 bits on the M4 Pro, so the
`inputs.toml` pins it with `verify-1mi-medium-neon-2026-09-23`; a run over
another needle set needs its own split, which `check_inputs.py` reports. Regenerate
it when the kernels or the needle set change. Timed as two passes it sums to
within a fifth of the fused scan rather than partitioning it exactly.

`paper/specs/micro-1mi.toml` runs our prefilter (12 and 16 bits, pruned)
against `onpair/decode+memmem-hay`, `fsst/decode+memmem-hay`,
`uncompressed/direct+memmem-hay` and `fsst_like_tum/interp` at five chunk
sizes, on four columns of 1,048,576 rows each: ClickBench URLs, Amazon book
titles, DBpedia abstracts and `movie_companies.note`, the last 49.3% empty
rows. `memmem-hay` searches a chunk's whole payload in one pass and maps each
hit back to its row, so all three baselines decode and scan a chunk at a time.
Its needles come from the SA + LCP generator of
`experiments/matcher_selection`, 20 per grid cell and 4,265 in total. Run it on
an idle machine: a load average above 2 slows the single-threaded baselines
several times and shows as a coefficient of variation above 0.05 in the
`stddev_ns / mean_ns` column. `micro-1mi-fast.toml` is the same four columns
and the same grid cells with one needle per cell, 220 in total and about half
an hour, which `paper/gen-micro-data.sh --fast` runs. `--medium` takes the
ten-per-cell suites, 2,155 needles and about four and a half hours. All three
cover identical cells, so the smaller ones are strict subsets rather than
different samples, and a figure drawn from one has the same shape with fewer
points per cell.

Measured on 2026-09-22, and the three sizes scale from it: a run costs two
minutes of dictionary builds whatever its size, and the rest is proportional to
the needle count. The fast set took 29 minutes on the M4 Pro and 34 on the EPYC
per build, which puts the medium set at 4.5 and 5.3 hours and the full one at
8.8 and 10.4.

`gen-micro-data.sh` names its runs `micro-1mi[-fast|-medium]-<arch>-<date>`,
and `check_inputs.py --update` moves `inputs.toml` onto them where it pins an
older run of the same size and instruction set. The panels follow the runs:
four columns are drawn at `PANEL_4UP`, 0.24 of the text width.
`micro-breakdown` is skipped, with a note, when no run for its instruction set
is loaded.

Instruction sets: the matcher sweeps cover NEON, AVX2 and AVX-512; the micro
and verify runs cover NEON on the M4 Pro and AVX-512 on the EPYC only.
`micro-arch/` is one panel per instruction set, the M4 Pro and the AVX-512
build on the x86 box (`target/` there). The `[micro]` keys of `inputs.toml` name them, and `pf.slug` turns those
labels into the file names `6_evaluation.tex` includes - a label changed
without the `\includegraphics` line beside it leaves a stale panel in
`assets/`. The two decompress-first baselines are drawn flat and dashed at
their median (`BASELINES` there), without markers or a quartile band; plaintext
memmem is drawn as a curve like the compressed-domain systems.

`plot_matcher.py` draws both encodings into every panel of
`kernel-matcher/`: onpair16's u16 codes solid, the u8 sweep of the FSST port
(`../fsst/search/prefilter/scan/bench`) dashed, one colour per kernel. The u16
build has no plain nibble bitmap and the u8 build no byte table, so each has a
series the other does not; the u8 sweep does measure a byte table, and
`DROPPED` keeps it out. The u8 stream is the same ClickBench URL prefix
encoded with FSST; the catalog comes from onpair's `bench_needles` over that
stream. Both grids now span the same axes: the |R| series is read at the widest
width whose |R| disjoint code ranges fit the code space, 16 codes for u16 and
8 for u8 (`RANGE_WIDTH`), and the u8 point at |C| = 256 is the whole 256-symbol
dictionary, named by the sweep because the catalog stops at 192. An encoding with no sweep for an instruction set is drawn faint
from the fitted cost model, and the panel says so.
