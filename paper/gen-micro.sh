#!/usr/bin/env sh
# Runs the micro-benchmark, specs/micro/<size>.toml, and moves paper/config.py
# onto the new run where it pins an older one of the same name.
#
#   paper/gen-micro.sh [fast|medium|full] [neon|avx512]     default: full, from uname -m
#
# Three sizes of the same experiment, the same four columns and grid cells,
# differing only in needles per cell. Measured on the M4 Pro (EPYC), at the
# former five row groups; one row group takes less:
#
#   fast      1 per cell,    211 needles,  30 min      (35 min)
#   medium   10 per cell,  2,053 needles,  4.5 h       (5.3 h)
#   full     20 per cell,  4,052 needles,  8.8 h       (10.4 h)
#
# On x86 build with RUSTFLAGS="-C target-cpu=native" on an AVX-512 host, or
# the AVX-512 panel gets a default build's numbers.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT="$HERE/.."
SIZE=${1:-full}
case $SIZE in fast | medium | full) ;; *) echo "unknown size: $SIZE (fast, medium, full)" >&2; exit 2 ;; esac
if [ $# -gt 1 ]; then ARCH=$2; else
  case $(uname -m) in arm64 | aarch64) ARCH=neon ;; *) ARCH=avx512 ;; esac
fi
case $ARCH in neon | avx512) ;; *) echo "unknown instruction set: $ARCH (neon, avx512)" >&2; exit 2 ;; esac

NAME="micro-$SIZE-$ARCH-$(date +%F)"
(cd "$ROOT" && cargo build --release --bin bench)
"$ROOT/target/release/bench" run "$HERE/specs/micro/$SIZE.toml" ${MIN_ITERS:+--min-iters "$MIN_ITERS"} -o "$ROOT/results/paper/$NAME"
# The check exits non-zero while an input is still missing or mixed, which is
# not a failed run.
(cd "$ROOT" && uv run paper/check_inputs.py --update) || true
echo "-> run $NAME"
