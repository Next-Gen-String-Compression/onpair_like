# paper

Data and figures for the kernel section of the substring-search preprint
(`../../substring_search_preprint`). Nothing here is built by the workspace.

| step | command | output |
| --- | --- | --- |
| stage one, stage two sweeps | `paper/gen-kernel-data.sh` | `paper/data/{matcher,resolver}_<arch>_<date>.csv` |
| stage one sweep of the FSST port | `paper/gen-kernel-data-fsst.sh` | `paper/data/matcher_fsst_<arch>_<date>.csv` |
| walk on/off end to end | `paper/gen-e2e-data.sh` | a run in `results/bench.duckdb` |
| figures | `uv run paper/plot_matcher.py`, `plot_resolver.py`, `plot_verify.py --run <name>` | `assets/kernel-{matcher,resolver,verify-rows,verify-e2e}.pdf` in the preprint |
| FSST figures | `uv run paper/plot_matcher.py --encoding fsst` | `assets/kernel-matcher-fsst/` in the preprint |
| micro-benchmark | `./target/release/bench run paper/specs/micro.toml -o results/paper/micro-<date>`, then `uv run paper/plot_micro.py --run micro-<date>` | `assets/micro-{needle,rowgroup}.pdf` |

The plot scripts read every CSV in `data/` unless given `--data`, and write
to the preprint's `assets/` unless given `--out`. A multi-panel figure writes **one PDF
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

`uv run paper/check_panels.py` enforces the second rule: it reads every
`\includegraphics` in `sections/`, works out the width that slot prints at,
compares it with the PDF's MediaBox and exits non-zero on any scale off 1.00. `plot_matcher.py` draws one
column of panels per instruction set, read off each CSV's `machine` column;
an instruction set with no rows gets the fitted cost model drawn dashed as a
placeholder. `plot_verify.py` writes two figures, one panel per machine each, given as
`--run "<label>=<run name>"`.

`data/` holds one sweep per instruction set, all from the current kernels
(2026-09-10): NEON on the M4 Pro, AVX2 and AVX-512 on the Xeon.
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

`plot_verify.py` reads four `onpair_spiral` strategies: `pf_walk` (the probe
cover walked, rows exact), `pf_memmem_superset` and `pf_kmp_superset` (the
same cover with the walk off, each verifier running) and `kmp`. Every per-row
cost is a difference inside one cell rather than across runs: the candidate
times the cover scanned both ways in its instrumented pass, so the walk is
`scan_ns - prefilter_ns` over `prefilter_candidates`, and each verifier is its
own `verify_ns` with the token automaton's table build charged to `setup_ns`.
`kernel-verify-rows` is a bar per verifier at the median over the needles,
each needle a point; a needle whose walk delta lands under the timer's
resolution (a few thousand candidate rows) shows near or below zero, and one
past the axis floor as a caret on it.

The micro-benchmark spec runs our prefilter (12 and 16 bits, pruned) against
`onpair/decode+memmem` and `fsst_like_tum/interp` on the three contains suites
(URL, IMDb names, DBpedia abstracts) at four chunk sizes; about five minutes
on the M4 Pro. Run it on an idle machine: a load average above 2 slows the
single-threaded baselines several times and shows as a coefficient of
variation above 0.05 in the `stddev_ns / mean_ns` column. The
`imdb-akaname-contains-s42` suite was generated for it with
`bench gen --seed 42 --ops contains` and blessed.

`plot_matcher.py --encoding fsst` draws the u8 sweep of the FSST port
(`../fsst/search/prefilter/scan/bench`) into `kernel-matcher-fsst/`, with the
universal nibble bitmap as a fifth series. Its stream is the same ClickBench
URL prefix encoded with FSST; the catalog comes from onpair's `bench_needles`
over that stream. At u8 width sixteen-code ranges fit only up to R = 16, so
the R panel ends there.
