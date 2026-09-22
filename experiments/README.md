# Experiments

Each experiment in this directory owns its policy, configuration, scripts,
documentation, and test fixtures. Reusable needle generation lives in
[`lb_harness::gen`](../harness/src/gen/README.md). Large reproducible caches and
generated outputs stay inside the experiment directory and are gitignored.

- [`optimize_prefilter`](optimize_prefilter/README.md): exact-selectivity query
  generation and benchmarking for the SpiralDB/OnPair prefilter.
- [`matcher_selection`](matcher_selection/README.md): full-column SA/LCP query
  preparation, independent verification, coverage reports and fixed-cover
  matcher benchmarks on the same three datasets as `optimize_prefilter`.
