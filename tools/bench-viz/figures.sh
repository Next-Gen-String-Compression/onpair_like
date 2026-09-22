#!/bin/bash
# The paper's LIKE figures, from Benchmark Explorer 3000™, in one command
# (TODO_like_workload.md Phase G). Each explorer is a self-contained HTML
# file: open it, pick X = Selectivity, Y = Throughput, Aggregate = Suite
# bucket, then facet by Pattern class; the PNG button exports the current view.
#
#   tools/bench-viz/figures.sh            # both runs, if their results exist
#   tools/bench-viz/figures.sh quick      # the development run only
#
# Outputs land in tools/bench-viz/out/ (gitignored: figures are derived from
# results/, which is itself derived and never committed).
set -euo pipefail
cd "$(dirname "$0")/../.."
OUT=tools/bench-viz/out
mkdir -p "$OUT"

build() { # run-dir title out-name [extra args...]
  local run=$1 title=$2 name=$3; shift 3
  if [ ! -f "$run/results.jsonl" ]; then
    echo "skip: $run has no results.jsonl (run its spec first)"; return
  fi
  python3 tools/bench-viz/bench_viz.py "$run" --queries suites \
    --title "$title" --out "$OUT/$name.html" "$@"
  echo "wrote $OUT/$name.html"
}

case "${1:-all}" in
  quick)
    build results/like-quick "LIKE quick loop" like-quick ;;
  all)
    build results/paper/like-cross-dataset \
      "LIKE throughput vs selectivity — ClickBench, IMDb, TPC-H" like-cross-dataset \
      --show onpair --show uncompressed --show fsst_like_tum
    build results/paper/tum-like-adapted \
      "Adapted TUM LIKE workload" tum-like-adapted \
      --show fsst_like_tum --show uncompressed --show fsst ;;
  *) echo "usage: $0 [all|quick]"; exit 1 ;;
esac
