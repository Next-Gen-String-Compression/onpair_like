#!/usr/bin/env sh
# Runs onpair's stage-one sweep, the matcher figure's input, and copies the
# CSV into paper/data under a stable name, from third_party/onpair. The URL corpus and its needle catalogs are built
# first when missing: bench_corpus over a ch.duckdb of the first two ClickBench
# parquet files (the duckdb CLI on PATH), then bench_needles over each OnPair
# stream; the corpus is the first 2^20 rows, which those two files hold in
# table order. The files come from datasets/raw/clickbench/, where
# datasets/prepare.py and e2e/data.sh keep them too, and are downloaded there
# when missing; ch.duckdb is built in target/kernel-corpus/.
#
#   paper/gen-kernel.sh
#
# The AVX-512 kernels are compile-time gated: on x86 run once plainly (the CSV
# reports avx2) and once under RUSTFLAGS="-C target-cpu=native", then rename
# the second CSV matcher_x86_64_avx512_<date>.csv.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ONPAIR=$(cd "$HERE/../third_party/onpair" && pwd)
# Linux calls arm64 aarch64; the sweeps take the name config.py and inputs.py look for.
MACHINE=$(uname -m | sed 's/^aarch64$/arm64/')
DATE=$(date +%F)
DATA="$ONPAIR/bench/data/ch/hits"
RAW="$HERE/../datasets/raw/clickbench"
CORPUS="$HERE/../target/kernel-corpus"

if [ ! -f "$DATA/URL_1m.onpair16.csv" ]; then
  if [ ! -e "$CORPUS/ch.duckdb" ]; then
    url=https://datasets.clickhouse.com/hits_compatible/athena_partitioned
    mkdir -p "$RAW" "$CORPUS"
    for i in 0 1; do
      # Below 100 kB/s for a minute the connection is cut and retried,
      # resuming; when resuming keeps failing the file starts over once.
      [ -s "$RAW/hits_$i.parquet" ] || {
        curl --fail --location --retry 3 --speed-limit 100000 --speed-time 60 --continue-at - \
            --output "$RAW/hits_$i.parquet.part" "$url/hits_$i.parquet" || {
          rm -f "$RAW/hits_$i.parquet.part"
          curl --fail --location --retry 3 --speed-limit 100000 --speed-time 60 \
              --output "$RAW/hits_$i.parquet.part" "$url/hits_$i.parquet"
        }
        mv "$RAW/hits_$i.parquet.part" "$RAW/hits_$i.parquet"
      }
    done
    rm -f "$CORPUS/ch.duckdb.tmp"
    duckdb "$CORPUS/ch.duckdb.tmp" -c "CREATE TABLE hits AS SELECT * FROM read_parquet(['$RAW/hits_0.parquet', '$RAW/hits_1.parquet'], binary_as_string = true)"
    mv "$CORPUS/ch.duckdb.tmp" "$CORPUS/ch.duckdb"
  fi
  # bench_corpus attaches ~/ch.duckdb, so HOME is the corpus folder for it
  # alone, with cargo's and rustup's homes left where they are.
  CORPUS=$(cd "$CORPUS" && pwd)
  (cd "$ONPAIR" && CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
    HOME="$CORPUS" cargo run --release --example bench_corpus -- ch)
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
