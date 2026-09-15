#!/usr/bin/env sh
# Runs the FSST port's stage-one and stage-two sweeps and copies their CSVs
# into paper/data.
# The stream and catalog must exist under fsst/search/prefilter/scan/bench/data:
# prefilter_corpus over the raw ch/hits/URL_1m.csv, then onpair's bench_needles
# example over the URL_1m.fsst.csv it writes.
#
#   paper/gen-kernel-data-fsst.sh [path/to/fsst]     default ../fsst
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
FSST=${1:-"$HERE/../../fsst"}
BUILD="$FSST/build-search"
MACHINE=$(uname -m)
DATE=$(date +%F)

cmake -S "$FSST" -B "$BUILD" -DCMAKE_BUILD_TYPE=Release >/dev/null
cmake --build "$BUILD" --target prefilter_matcher_sweep prefilter_resolver_sweep >/dev/null

run() { # <sweep binary> <csv prefix> <target name>
  "$BUILD/search/$1"
  src=$(ls -t "$FSST"/search/prefilter/scan/bench/output/"$2"_*.csv | head -1)
  cp "$src" "$HERE/data/$3_fsst_${MACHINE}_${DATE}.csv"
  echo "-> paper/data/$3_fsst_${MACHINE}_${DATE}.csv"
}

run prefilter_matcher_sweep  novel_mask    matcher
run prefilter_resolver_sweep novel_resolve resolver
