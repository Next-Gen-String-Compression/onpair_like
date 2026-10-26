#!/usr/bin/env sh
# The matcher figure from a clone of this repository: runs both sweeps with their
# corpus steps, and draws kernel-matcher/ from the CSVs they wrote.
#
#   paper/reproduce-kernel.sh
#
# Needs a Rust toolchain, a C++ compiler, cmake, the duckdb CLI and uv; the
# figure also needs TeX (paper/check-tex.sh), and without it only the sweeps
# run, for make-figures.sh to draw from. OnPair and the FSST fork
# are third_party/onpair and third_party/fsst. On an AVX-512 host the onpair sweep
# runs twice, the native build first, so both x86 columns are measured.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
. "$ROOT/pins.env"
DATE=$(date +%F)
# Linux calls arm64 aarch64; the sweeps take the name config.py and inputs.py look for.
MACHINE=$(uname -m | sed 's/^aarch64$/arm64/')

ONPAIR="$ROOT/third_party/onpair"

AVX512=
[ "$MACHINE" = x86_64 ] && grep -q avx512bw /proc/cpuinfo 2>/dev/null && AVX512=1

if [ -n "$AVX512" ]; then
  RUSTFLAGS="-C target-cpu=native" CARGO_TARGET_DIR="$ONPAIR/target-native" sh "$HERE/gen-kernel.sh"
  mv "$HERE/data/matcher_${MACHINE}_$DATE.csv" "$HERE/data/matcher_${MACHINE}_avx512_$DATE.csv"
fi
sh "$HERE/gen-kernel.sh"

sh "$HERE/gen-kernel-fsst.sh"
if [ -n "$AVX512" ]; then
  mv "$HERE/data/matcher_fsst_${MACHINE}_$DATE.csv" "$HERE/data/matcher_fsst_${MACHINE}_avx512_$DATE.csv"
fi

OUT="$HERE/out/kernel-$DATE"
if sh "$HERE/check-tex.sh"; then
  (cd "$ROOT" && uv run paper/plot_matcher.py --data paper/data/matcher_"$MACHINE"_*"$DATE".csv paper/data/matcher_*_"$MACHINE"_*"$DATE".csv --out "$OUT")
  echo "-> $OUT/kernel-matcher/"
else
  echo "kernel figure not drawn here; paper/make-figures.sh draws it from the sweeps"
fi
