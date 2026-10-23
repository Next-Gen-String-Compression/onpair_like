#!/usr/bin/env sh
# Runs the FSST port's stage-one sweep, the matcher figure's input, and copies
# the CSV into paper/data. The FSST fork is third_party/fsst.
# The stream and catalog are built first when missing: prefilter_corpus over
# onpair's raw ch/hits/URL_1m.csv (gen-kernel.sh writes it), then onpair's
# bench_needles example over the URL_1m.fsst.csv it writes.
#
#   paper/gen-kernel-fsst.sh
#
# The build is -march=native, so on x86 the CSV carries the host's widest
# instruction set; name an AVX-512 host's CSV matcher_fsst_x86_64_avx512_<date>.csv.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
FSST=$(cd "$HERE/../third_party/fsst" && pwd)
ONPAIR=$(cd "$HERE/../third_party/onpair" && pwd)
BUILD="$HERE/../target/fsst-search"
MACHINE=$(uname -m)
DATE=$(date +%F)
DATA="$FSST/search/prefilter/scan/bench/data"

cmake -S "$FSST" -B "$BUILD" -DCMAKE_BUILD_TYPE=Release >/dev/null
cmake --build "$BUILD" --target prefilter_corpus prefilter_matcher_sweep >/dev/null

corpus() { # <db/table/column_size>: encode onpair's raw stream and draw its catalog
  raw="$ONPAIR/bench/data/$1.csv"
  out="$DATA/$1.fsst.csv"
  [ -f "$out" ] || { mkdir -p "$(dirname "$out")"; "$BUILD/search/prefilter_corpus" "$raw" "$out"; }
  [ -f "$DATA/$1.fsst.needles.csv" ] || (cd "$ONPAIR" && cargo run --release --example bench_needles -- "$out")
}
[ -f "$ONPAIR/bench/data/ch/hits/URL_1m.csv" ] || { echo "onpair's ch/hits/URL_1m.csv missing: run paper/gen-kernel.sh first" >&2; exit 1; }
corpus ch/hits/URL_1m

run() { # <sweep binary> <csv prefix> <target name>
  "$BUILD/search/$1"
  src=$(ls -t "$FSST"/search/prefilter/scan/bench/output/"$2"_*.csv | head -1)
  cp "$src" "$HERE/data/$3_fsst_${MACHINE}_${DATE}.csv"
  echo "-> paper/data/$3_fsst_${MACHINE}_${DATE}.csv"
}

run prefilter_matcher_sweep novel_mask matcher
