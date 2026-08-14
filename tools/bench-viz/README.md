# bench-viz

An interactive, single-plot viewer for `LIKE-benchmark` result files. It builds
one self-contained HTML file: no Python packages, JavaScript packages, server,
or network access are required after generation.

The viewer can:

- toggle any query strategy independently;
- add candidate-specific decode-only baselines derived from the harness's
  instrumented `decode_ns` phase;
- switch either axis between linear and logarithmic scaling;
- focus the x-axis on an inclusive interval (percent for selectivity, bytes for
  needle length), excluding out-of-range queries before aggregation;
- fit the y-axis to only the currently visible query lines and decode baselines;
- change dataset, operation, chunking, axes, aggregation, title, subtitle, and
  axis labels without regenerating the file;
- show raw observations, median lines, and interquartile bands; and
- export the current view to a 1×–4× PNG (up to 4800 pixels wide).

## Build a viewer

From the repository root:

```sh
python3 tools/bench-viz/bench_viz.py \
  results/my-run/results.jsonl \
  --show onpair --show uncompressed \
  --title "Contains throughput" \
  --out tools/bench-viz/out/my-run.html
```

A run directory can be passed in place of its `results.jsonl`. Multiple result
files are accepted and appear in the **Run** selector.

Open the generated HTML directly in a browser. The PNG button serializes the
current SVG and rasterizes it locally, so the exported image includes the
current scales, selected series, labels, and decode baselines.
