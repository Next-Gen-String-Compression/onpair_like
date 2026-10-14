#!/usr/bin/env sh
# Runs the row group / pre-selection block on the AVX-512 box: the row group
# sweep, its LLVM part, and the pre-selection, specs/rows/*.toml, then moves
# paper/config.py onto the new runs where it pins older ones of the same name.
#
#   paper/gen-rows.sh
#
# x86 only: `llvm-simd` registers only on an x86 build against LLVM 14-16.
# Build with RUSTFLAGS="-C target-cpu=native".
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT="$HERE/.."
case $(uname -m) in x86_64) ;; *) echo "gen-rows.sh runs on the AVX-512 box only" >&2; exit 2 ;; esac

DATE=$(date +%F)
(cd "$ROOT" && cargo build --release --bin bench)
for spec in rowgroup rowgroup-llvm selection; do
  "$ROOT/target/release/bench" run "$HERE/specs/rows/$spec.toml" -o "$ROOT/results/paper/$spec-fast-avx512-$DATE"
done
(cd "$ROOT" && uv run paper/check_inputs.py --update) || true
echo "-> runs rowgroup-fast-avx512-$DATE, rowgroup-llvm-fast-avx512-$DATE, selection-fast-avx512-$DATE"
