#!/usr/bin/env sh
# Runs the row group / pre-selection block, specs/rows/*.toml: on the AVX-512
# box the row group sweep, its LLVM part and the pre-selection; on arm64 the
# NEON row group sweep, rowgroup-neon.toml. Then moves
# paper/config.py onto the new runs where it pins older ones of the same name.
#
#   paper/gen-rows.sh
#
# The LLVM part is x86 only: `llvm-simd` registers only on an x86 build
# against LLVM 14-16.
# Build with RUSTFLAGS="-C target-cpu=native".
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT="$HERE/.."
DATE=$(date +%F)
(cd "$ROOT" && cargo build --release --bin bench)
case $(uname -m) in
  x86_64)
    for spec in rowgroup rowgroup-llvm selection; do
      "$ROOT/target/release/bench" run "$HERE/specs/rows/$spec.toml" ${MIN_ITERS:+--min-iters "$MIN_ITERS"} -o "$ROOT/results/paper/$spec-fast-avx512-$DATE"
    done
    RUNS="rowgroup-fast-avx512-$DATE, rowgroup-llvm-fast-avx512-$DATE, selection-fast-avx512-$DATE" ;;
  arm64|aarch64)
    "$ROOT/target/release/bench" run "$HERE/specs/rows/rowgroup-neon.toml" ${MIN_ITERS:+--min-iters "$MIN_ITERS"} -o "$ROOT/results/paper/rowgroup-fast-neon-$DATE"
    RUNS="rowgroup-fast-neon-$DATE" ;;
esac
(cd "$ROOT" && uv run paper/check_inputs.py --update | grep -v '^missing') || true
echo "-> runs $RUNS"
