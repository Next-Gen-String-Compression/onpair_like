#!/usr/bin/env sh
# Runs onpair's stage-one, stage-two and walk benches and copies the CSV each
# writes into paper/data under a stable name. Corpora must exist under
# onpair/bench/data (cargo run --release --example bench_corpus, then
# bench_needles).
#
#   paper/gen-kernel-data.sh [path/to/onpair]     default ../onpair
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ONPAIR=${1:-"$HERE/../../onpair"}
MACHINE=$(uname -m)
DATE=$(date +%F)

run() { # <bench path> <csv prefix> <target name>
  (cd "$ONPAIR" && cargo test --release --lib "$1" -- --ignored --nocapture)
  src=$(ls -t "$ONPAIR"/bench/output/"$2"_*.csv | head -1)
  cp "$src" "$HERE/data/$3_${MACHINE}_${DATE}.csv"
  echo "-> paper/data/$3_${MACHINE}_${DATE}.csv"
}

# Test paths as of onpair's 2026-09-10 scan refactor, which moved the sweeps
# under `substring::prefilter::scan::bench` and renamed them to `sweep`. The
# walk sweep (`prefilter::bench::walk_cost`) is gone with that refactor: the
# walk runs inside the scan now and has no standalone cost bench, so `walk_*`
# CSVs cannot be regenerated on a new machine.
run scan::bench::matcher_fit::sweep   novel_mask    matcher
run scan::bench::resolver_fit::sweep  novel_resolve resolver
