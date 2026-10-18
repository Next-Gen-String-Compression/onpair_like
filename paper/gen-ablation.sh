#!/usr/bin/env sh
# Runs the SIMD-cut ablations, specs/ablation/<size>.toml: simd_cut_onpair_debug (OnPair12,
# OnPair16) and simd_cut_fsst_debug with the symbol table sorted or not and planned with
# the frequency counts or uniform frequencies, every variant in one run.
#
#   paper/gen-ablation.sh [fast|medium]     default: medium
#
# Measured on the M4 Pro: fast 211 needles, about 2 min; medium 2,053 needles, about 20 min.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT="$HERE/.."
SIZE=${1:-medium}
case $SIZE in fast | medium) ;; *) echo "unknown size: $SIZE (fast, medium)" >&2; exit 2 ;; esac
case $(uname -m) in arm64 | aarch64) ARCH=neon ;; *) ARCH=avx512 ;; esac

NAME="ablation-$SIZE-$ARCH-$(date +%F)"
(cd "$ROOT" && cargo build --release --bin bench)
"$ROOT/target/release/bench" run "$HERE/specs/ablation/$SIZE.toml" ${MIN_ITERS:+--min-iters "$MIN_ITERS"} -o "$ROOT/results/paper/$NAME"
echo "-> run $NAME"
