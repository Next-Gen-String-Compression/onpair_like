#!/usr/bin/env sh
# The matcher figure from a clone of this repository: fetches the two kernel
# repos at their pins when they are missing, runs both sweeps with their
# corpus steps, and draws kernel-matcher/ from the CSVs they wrote.
#
#   paper/reproduce-kernel.sh
#
# Needs a Rust toolchain, a C++ compiler, cmake, the duckdb CLI, uv and a TeX
# install with libertine, zi4 and newtxmath on PATH. Layout: ../onpair beside
# this repository, and the FSST fork at ../duckdb/third_party/fsst/upstream
# when that checkout exists, else ../fsst. On an AVX-512 host the onpair sweep
# runs twice, the native build first, so both x86 columns are measured.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
. "$ROOT/pins.env"
DATE=$(date +%F)
MACHINE=$(uname -m)

ONPAIR="$ROOT/../onpair"
[ -d "$ONPAIR" ] || { git clone -q "$ONPAIR_URL" "$ONPAIR" && git -C "$ONPAIR" checkout -q "$ONPAIR_SHA"; }
FSST="$ROOT/../duckdb/third_party/fsst/upstream"
[ -d "$FSST" ] || FSST="$ROOT/../fsst"
[ -d "$FSST" ] || { git clone -q "$FSST_URL" "$FSST" && git -C "$FSST" checkout -q "$FSST_SHA"; }

AVX512=
[ "$MACHINE" = x86_64 ] && grep -q avx512bw /proc/cpuinfo 2>/dev/null && AVX512=1

if [ -n "$AVX512" ]; then
  RUSTFLAGS="-C target-cpu=native" CARGO_TARGET_DIR="$ONPAIR/target-native" "$HERE/gen-kernel.sh" "$ONPAIR"
  mv "$HERE/data/matcher_${MACHINE}_$DATE.csv" "$HERE/data/matcher_${MACHINE}_avx512_$DATE.csv"
fi
"$HERE/gen-kernel.sh" "$ONPAIR"

"$HERE/gen-kernel-fsst.sh" "$FSST" "$ONPAIR"
if [ -n "$AVX512" ]; then
  mv "$HERE/data/matcher_fsst_${MACHINE}_$DATE.csv" "$HERE/data/matcher_fsst_${MACHINE}_avx512_$DATE.csv"
fi

OUT="$HERE/out/kernel-$DATE"
(cd "$ROOT" && uv run paper/plot_matcher.py --data paper/data/matcher_"$MACHINE"_*"$DATE".csv paper/data/matcher_*_"$MACHINE"_*"$DATE".csv --out "$OUT")
echo "-> $OUT/kernel-matcher/"
