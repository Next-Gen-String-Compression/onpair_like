#!/usr/bin/env sh
# Runs onpair's stage-one sweep, the matcher figure's input, and copies the
# CSV into paper/data under a stable name. The checkout must be clean at
# ONPAIR_SHA in pins.env. The URL corpus and its needle catalogs are built
# first when missing: bench_corpus over ~/ch.duckdb (the duckdb CLI on PATH),
# then bench_needles over each OnPair stream. The stage-two sweep, which no
# figure draws, runs only when its IMDb stream exists (bench_corpus imdb, over
# ~/imdb.duckdb, then bench_needles over imdb/name/name_1m.onpair16.csv).
#
#   paper/gen-kernel.sh [path/to/onpair]     default ../onpair
#
# The AVX-512 kernels are compile-time gated: on x86 run once plainly (the CSV
# reports avx2) and once under RUSTFLAGS="-C target-cpu=native", then rename
# the second CSV matcher_x86_64_avx512_<date>.csv.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ONPAIR=$(cd "${1:-"$HERE/../../onpair"}" && pwd)
. "$HERE/../pins.env"
[ "$(git -C "$ONPAIR" rev-parse HEAD)" = "$ONPAIR_SHA" ] && [ -z "$(git -C "$ONPAIR" status --porcelain --untracked-files=no)" ] \
  || { echo "$ONPAIR is not clean at ONPAIR_SHA $ONPAIR_SHA" >&2; exit 1; }
MACHINE=$(uname -m)
DATE=$(date +%F)
DATA="$ONPAIR/bench/data/ch/hits"

if [ ! -f "$DATA/URL_1m.onpair16.csv" ]; then
  (cd "$ONPAIR" && cargo run --release --example bench_corpus -- ch)
fi
for enc in onpair12 onpair16; do
  if [ ! -f "$DATA/URL_1m.$enc.needles.csv" ]; then
    (cd "$ONPAIR" && cargo run --release --example bench_needles -- "$DATA/URL_1m.$enc.csv")
  fi
done

run() { # <bench path> <csv prefix> <target name>
  (cd "$ONPAIR" && cargo test --release --lib "$1" -- --ignored --nocapture)
  src=$(ls -t "$ONPAIR"/bench/output/"$2"_*.csv | head -1)
  cp "$src" "$HERE/data/$3_${MACHINE}_${DATE}.csv"
  echo "-> paper/data/$3_${MACHINE}_${DATE}.csv"
}

# The walk has no standalone sweep since onpair's 2026-09-10 scan refactor, so
# walk_*.csv cannot be regenerated; the walk's cost comes out of the harness run.
run scan::bench::matcher_fit::sweep   novel_mask    matcher
if [ -f "$ONPAIR/bench/data/imdb/name/name_1m.onpair16.needles.csv" ]; then
  run scan::bench::resolver_fit::sweep  novel_resolve resolver
else
  echo "no imdb/name/name_1m stream: resolver sweep skipped"
fi
