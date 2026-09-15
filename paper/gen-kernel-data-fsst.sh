#!/usr/bin/env sh
# Runs the FSST port's stage-one sweep and copies its CSV into paper/data.
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
cmake --build "$BUILD" --target prefilter_matcher_sweep >/dev/null
"$BUILD/search/prefilter_matcher_sweep"
src=$(ls -t "$FSST"/search/prefilter/scan/bench/output/novel_mask_*.csv | head -1)
cp "$src" "$HERE/data/matcher_fsst_${MACHINE}_${DATE}.csv"
echo "-> paper/data/matcher_fsst_${MACHINE}_${DATE}.csv"
