#!/usr/bin/env sh
# Runs the micro-benchmark on the 2^20-row roster, then the same roster's
# three verifiers, and moves paper/inputs.toml to the new runs where it pins an
# older run of the same size and instruction set; paper/make-figures.sh draws
# the figures. The two are separate specs because `strategies` filters every
# candidate, and pf_memmem_superset and pf_kmp_superset exist only on
# onpair_spiral; the verify spec adds a few minutes at any size, sweeping no
# row groups and no other candidate.
#
#   paper/gen-micro-data.sh [--fast|--medium] [neon|avx512]
#
# Three sizes of the same experiment: the same four columns and the same grid
# cells, differing only in how many needles fill a cell. Measured on the M4
# Pro, where a run's fixed cost is two minutes of dictionary builds and the
# rest scales with the needle count:
#
#   --fast      1 per cell,    220 needles,  30 min      (35 min on the EPYC)
#   --medium   10 per cell,  2,155 needles,  4.5 h       (5.3 h)
#   (default)  20 per cell,  4,265 needles,  8.8 h       (10.4 h)
#
# The instruction set names the run and picks the figure's panel, and defaults
# from `uname -m`: arm64 -> neon, x86_64 -> avx512. The micro figures cover
# NEON and AVX-512 only; AVX2 is measured for the matcher kernels alone
# (gen-kernel-data.sh). On x86 build with RUSTFLAGS="-C target-cpu=native" on
# an AVX-512 host, or the AVX-512 panel gets a default build's numbers.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT="$HERE/.."

SPEC="$HERE/specs/micro-1mi.toml"
VERIFY_SPEC="$HERE/specs/verify-1mi.toml"
TAG=micro-1mi
VERIFY_TAG=verify-1mi
case "${1:-}" in
  --fast)
    SPEC="$HERE/specs/micro-1mi-fast.toml"
    VERIFY_SPEC="$HERE/specs/verify-1mi-fast.toml"
    TAG=micro-1mi-fast
    VERIFY_TAG=verify-1mi-fast
    shift
    ;;
  --medium)
    SPEC="$HERE/specs/micro-1mi-medium.toml"
    VERIFY_SPEC="$HERE/specs/verify-1mi-medium.toml"
    TAG=micro-1mi-medium
    VERIFY_TAG=verify-1mi-medium
    shift
    ;;
esac

if [ $# -gt 0 ]; then
  ARCH=$1
else
  case $(uname -m) in
    arm64 | aarch64) ARCH=neon ;;
    *) ARCH=avx512 ;;
  esac
fi
case $ARCH in
  neon | avx512) ;;
  *) echo "unknown instruction set: $ARCH (neon, avx512)" >&2; exit 2 ;;
esac

NAME="$TAG-$ARCH-$(date +%F)"
VERIFY_NAME="$VERIFY_TAG-$ARCH-$(date +%F)"
LLVM_SPEC=$(echo "$SPEC" | sed 's|/micro-1mi|/micro-llvm-1mi|')
LLVM_NAME=$(echo "$TAG" | sed 's|^micro-1mi|micro-llvm-1mi|')-$ARCH-$(date +%F)
(cd "$ROOT" && cargo build --release --bin bench)
"$ROOT/target/release/bench" run "$SPEC" -o "$ROOT/results/paper/$NAME"
# The spec that gives micro-breakdown its LLVM JIT bar runs on the AVX-512
# build only: that figure draws the x86, AVX-512 machine alone, and the JIT
# backend registers only on x86 built against LLVM 14-16. It sweeps one row
# group and one candidate, minutes rather than hours.
if [ "$ARCH" = avx512 ]; then
  "$ROOT/target/release/bench" run "$LLVM_SPEC" -o "$ROOT/results/paper/$LLVM_NAME"
fi
"$ROOT/target/release/bench" run "$VERIFY_SPEC" -o "$ROOT/results/paper/$VERIFY_NAME"
# `bench run` loads each run into results/bench.duckdb itself. The check
# exits non-zero while an input is still missing or mixed, which is not a
# failed run, so its status is not the script's.
(cd "$ROOT" && uv run paper/check_inputs.py --update) || true
echo "-> runs $NAME and $VERIFY_NAME"
