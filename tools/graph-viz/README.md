# onpair-graph-viz

Draws how OnPair decides which dictionary tokens to probe for a `LIKE '%pattern%'`
query: the DAG of every way the pattern can lie across token boundaries, and the
minimum weighted vertex cut that becomes the probe cover.

![the figure this produces](tests/golden/newsletter.svg)

## What the picture says

A row can only contain the pattern if it holds at least one token from the cover,
so rows without one are never decoded. Two things can happen at a match:

* **One token contains the whole pattern.** Those ids are mandatory members of
  every cover. They are deliberately *not* in the DAG — such a match crosses no
  boundary, so no path stands for it and no cut could select it. They still count
  in `CUT TOKEN FREQUENCY`.
* **The match crosses a boundary.** Then it begins at some feasible first-token
  alignment `k`, and greedy parsing of the rest is deterministic. Each such layout
  is one path from an alignment to an accepting terminal. A **cut** of this DAG is
  therefore exactly a sound cover: whichever layout the occurrence takes, it runs
  into a probe.

Reading the figure:

| element | meaning |
|---|---|
| left column, one card per row | a feasible alignment `k` — `k` needle bytes sit at the tail of the first token |
| rounded `p=N` node | parsing has consumed the needle up to byte offset `N` |
| teal filled `p=N`, `merge ×m` | `m` alignments converged here; one probe downstream blocks all of them |
| callout card on an edge | the single interior token greedy parsing takes at that offset |
| card in the bottom row | an accepting terminal: tokens whose prefix is the whole remaining needle |
| **orange** outline, `CUT` badge | selected by the cut — this is the cover |
| faded node | downstream of the cut, so unreachable once the cover is in place |
| `TF` / `DF` | token occurrences in the code stream / rows holding the token |

`STATE VISITS → MERGED` is the payoff of merging by byte offset: how many states
the alignments would visit separately versus how many the DAG materializes.

Why a *global* cut rather than the cheapest probe per alignment: local choices pay
twice for alignments that converge, when one probe at the join blocks both, and
they weigh a first-token set against a single terminal when picking that set would
have obviated all of them. A cut has neither blind spot because it is not making a
sequence of local choices at all.

## Input

Anything that is an OnPair column. The library takes a `ColumnView` and the
`TokenFrequencyIndex` built for it — no dataset, benchmark, or workload is baked
in:

```rust
let view = column.view();
let frequencies = build_token_frequency_index(view.codes, view.dict.num_tokens())?;
let figure = visualize(view, &frequencies, b"utm_source=", &Options::default())?;
std::fs::write("figure.svg", figure.svg.unwrap())?;
```

`Figure` also carries the graph, the cut, the cover's SIMD cost, and the
measurements below — it serializes to JSON as-is.

The binary is glue for the common case, turning a text file into a column:

```console
$ cargo run --release -- \
    --rows urls.txt --title "clickbench urls" --out figures --gallery \
    --pattern 'utm_source=newsletter' --pattern '/cart/checkout'
column: 40000 rows, 181659 codes, 937 dictionary tokens
  01-utm-source-newsletter: 4 alignments, 4 states, cut 3 probes weight 2432 · 2289 candidates (866 exact), onpair 2289, sound true
  02-cart-checkout: 3 alignments, 3 states, cut 3 probes weight 4431 · 5571 candidates (5571 exact), onpair 5571, sound true
```

One row per line; `--rows -` reads stdin. `--help` lists the rest: `--metric` for
the cut objective (`tf`, `tf_residual`, `df`, `df_residual`), `--pattern-hex` for
non-UTF-8 needles, `--no-measure`, `--max-states`.

## It checks itself

This crate rebuilds the DAG instead of reading OnPair's. That is on purpose —
OnPair's planner keeps only ids and weights because it runs per query, while a
figure needs token strings, provenance and labels, and the library must not carry
that. The cost is a second implementation of the same logic, which could drift and
draw something false.

So every figure is measured against the column it came from: candidate rows the
cover admits, true matches by decode-and-search, and what OnPair's own
`prefilter_candidates` admits for the same pattern. `sound` is false if any true
match fell outside the cover, and the binary exits non-zero when that happens.
Differing candidate counts are reported but not treated as failure — several cuts
can tie on weight, and both would be sound.

That check has a blind spot, and it is worth knowing where. It compares *rows*, so
it catches any divergence that changes which rows are admitted — but not one that
only changes the probes. Zero-frequency pruning is exactly that kind: OnPair drops
cut probes whose tokens occur nowhere in the column, which cannot change a
candidate set, so a figure still drawing them would measure as perfectly sound
while showing comparisons the scan never issues. `live_cover` mirrors that rule by
hand and the figures mark what it drops (grey, dashed, `PRUNED`).

**So when the `onpair` pin in `Cargo.toml` moves, re-read the planner.** The two
functions this crate copies from it are flagged at their definitions:
`graph::live_cover` (run merging plus the zero-frequency trim) and `mincut`.

## Notes

* **Size guard.** Each byte-offset state is about 148 units of width, so a long
  needle produces a technically valid and practically useless figure. Past
  `--max-states` (default 128) the JSON is still written and the SVG is skipped.
* **Fonts.** The SVG is self-contained — inline styles, no external references —
  but names a system font stack rather than embedding a face. Card widths are
  fixed, so a viewer without Inter or a similar UI font may set text slightly
  wider than the layout assumed. Embedding a face would add roughly 200 KB per
  figure; converting to PDF once at publication time is the better trade.
* **Golden figure.** `tests/golden/newsletter.svg` is byte-compared on every test
  run. Regenerate with `UPDATE_GOLDEN=1 cargo test`, and open the diff — a layout
  regression looks exactly like a layout improvement until someone does.
* **Own workspace.** This crate pins a newer OnPair revision than the benchmark
  candidates, and Cargo will not resolve two revisions of one git repository in a
  single lockfile.
