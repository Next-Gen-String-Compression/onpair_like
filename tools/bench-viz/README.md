# Benchmark Explorer 3000™

An interactive, single-plot viewer for `LIKE-benchmark` result files—making
today's regressions tomorrow's problem. It builds
one self-contained HTML file: no Python packages, JavaScript packages, server,
or network access are required after generation.

The viewer can:

- toggle any query strategy independently;
- add candidate-specific decode-only baselines derived from the harness's
  instrumented `decode_ns` phase;
- switch either axis between linear and logarithmic scaling;
- focus the x-axis on an inclusive interval (percent for selectivity, bytes for
  needle length), excluding out-of-range queries before aggregation;
- fit the y-axis to only the currently visible candidate series and decode baselines;
- change dataset, operation, chunking, axes, aggregation, title, subtitle, and
  axis labels without regenerating the file;
- show raw observations, median lines, and interquartile bands;
- click a raw point to highlight one query, or a median point to select every
  query in that aggregate, then inspect needles, result statistics, mincut cover
  shape/frequency, profitability facts, timing distributions, and provenance in
  the collapsible panel below the plot;
- optionally embed the full parsing DAG and highlighted minimum cut inside each
  query row, collapsed and loaded only on demand, with fit-width/actual-size
  views and SVG download; and
- export the current view to a 1×–4× PNG (up to 4800 pixels wide).

## Build a viewer

From the repository root:

```sh
python3 tools/bench-viz/bench_viz.py \
  results/my-run/results.jsonl \
  --queries suites/my-suite \
  --show onpair --show uncompressed \
  --mincut-graphs \
  --title "Contains throughput" \
  --out tools/bench-viz/out/my-run.html
```

A run directory can be passed in place of its `results.jsonl`. Multiple result
files are accepted and appear in the **Run** selector.

The harness stores a query id in each repeated candidate measurement, not the
needle bytes themselves. The builder discovers `queries.jsonl` files beneath a
named run directory and follows suite paths in an adjacent `manifest.json`.
Use repeatable `--queries` options to add or override suite locations when a run
was copied without its original suite. The detail panel still works without a
suite, but reports that the needle bytes are unavailable.

`--mincut-graphs` uses `tools/graph-viz` to draw each DAG. ABI-v7 runs export the
exact token dictionary and frequency index in a deterministic replay phase
after the complete measurement matrix; when an artifact row is present, it is
authoritative and the explorer never re-trains. Graphs are grouped by the full
measured build identity, so several
OnPair candidates and dictionary sizes can coexist in one explorer. Legacy
runs without sidecars retain the stricter reconstruction fallback: a
fingerprint must agree when available, and older `onpair_spiral` results are
accepted only after every recorded single-needle cover matches. Chunked cells
are omitted because they contain several independently trained dictionaries,
and a run with no compatible candidate simply embeds no graphs—no hypothetical
dictionary is made.

The replay phase runs after every candidate's measurements, so chunk-size
sweeps can export all of their independently built sidecars without affecting
timings. The current explorer still omits chunked cells rather than pretending
that several per-chunk dictionaries are one whole-column graph.

The builder discovers datasets through the run manifest and remaps paths from
other checkouts onto `datasets/` here; use repeatable
`--mincut-dataset DATASET=PATH` overrides when necessary. Each compressed bundle
is loaded only when its collapsed graph row is opened, so selecting a median
with hundreds of queries does not eagerly parse or mount hundreds of SVGs.

Clicking a raw point selects its query across every visible candidate. Clicking
an aggregate point selects all queries contributing to that median. Hold Shift,
Ctrl, or Command while clicking to compare several selections; the persistent
outlines remain visible even when raw scatter is hidden.

Open the generated HTML directly in a browser. The PNG button serializes the
current SVG and rasterizes it locally, so the exported image includes the
current scales, selected series, labels, and decode baselines.

## The Prefilter section

A second tab, sharing the same selection as the throughput panel — run,
dataset, operation, chunking, focus range and visible candidates — because
switching tabs should change how the data is described, never which data.

The throughput panel answers "how fast was this series". Every question a
compressed-domain prefilter actually raises is a comparison against the
fallback the engine would otherwise run, *on the same needle*, so this section
is built on paired per-query ratios against a baseline series you choose.
It defaults to the slowest visible series, which on these runs is the
decode-then-scan fallback.

| Panel | Answers |
|---|---|
| **The column** | Codes per row, payload bytes per code, κ, and the run's own noise floor |
| **Against the baseline** | Win rate, p10/median/p90 speedup, worst regression, and total time both ways |
| **Profitability gate** | What a policy decides here, scored by the time it costs; plus a threshold sweep |
| **The frontier** | Cover width against verification load, coloured by outcome, with the gate drawn |
| **The cost model** | Predicted against measured, fitted constants per series, and an R² leaderboard |
| **The numbers** | The table behind the current cut, with n per bin, copyable as TSV |

Two things are worth knowing about how this is computed.

**Statistics live in Python.** `prefilter_model.py` derives the column shape,
fits the cost model, and finds the noise floor at build time; the results are
embedded in the page and the viewer only reduces what is on screen. The
statistics are therefore unit tested without a browser.

**The noise floor is found, not declared.** A spec that lists one candidate
twice at configs that parse identically has two worker processes doing the same
work, and their spread is the run's resolution. Those configs cannot be
recognised by their key — the harness hashes the config *string* — so they are
matched by having produced a byte-identical column, and the matched pair is
reported alongside the figure so the inference is auditable.

**Column shape is recovered from older runs.** `num_rows` and `payload_bytes`
on the build record are recent, but both are exactly recoverable from any query
row: `gbps_raw` is payload bytes per nanosecond by construction, and
`ns_per_value` is the median over the row count. Results already on disk work.

## Tests

```sh
# statistics and normalization
python3 -m pytest tools/bench-viz/tests

# viewer reductions and the render path, under the JavaScriptCore shell macOS ships
JSC=/System/Library/Frameworks/JavaScriptCore.framework/Versions/A/Helpers/jsc
$JSC tools/bench-viz/tests/prefilter.test.js
$JSC tools/bench-viz/tests/render.test.js

# and, against a real run, that the viewer reproduces the published figures
python3 tools/bench-viz/tests/extract_payload.py <explorer.html> /tmp/payload.json
$JSC tools/bench-viz/tests/render.test.js  -- /tmp/payload.json
$JSC tools/bench-viz/tests/figures.test.js -- /tmp/payload.json
```

`figures.test.js` checks the viewer's reductions against numbers computed
independently in Python by
`experiments/optimize_prefilter/analysis/predict_profitability.py`. Two
implementations of the same statistics, in different languages, agreeing to
four digits is stronger evidence than either passing its own unit tests.

It is opt-in rather than part of the suite above: it needs a `needle-sweep` run
with cover facts/artifact fingerprints and a decode baseline, and re-running the Python side
needs numpy. Given neither, the figures it pins still guard the JavaScript
reductions against drift.
