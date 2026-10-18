#!/bin/bash
# Reproduces the paper's x86 measurements on an AVX-512 Linux host (the EPYC
# 9R14 of a c7a.xlarge), end to end from a clone: sibling repos at their pins,
# build and tests, the four columns, the needle suites, and every block the
# paper runs on AVX-512, with the kernel sweep on AVX2 as well.
# reproduce-neon.sh measures the arm64 side; with both machines' results/ and
# paper/data in one tree, paper/make-figures.sh draws every figure.
#
#   ./reproduce-avx512.sh [--small|--medium|--large] [--min-iters=N]
#       default: --small, each spec's own min_iters
#
# The size picks the needle set, the same four columns and grid cells with
# 1, 10 or 20 needles per cell. The micro run alone takes about 0.6, 5.3 and
# 10.4 hours. --min-iters=N sets every harness run's timed iterations per cell.
#
# Needs a Rust toolchain, a C++ compiler, cmake, python3, uv, the duckdb CLI,
# and LLVM 14-16 for the `llvm-simd` baseline. Downloads about 7 GB for the
# four columns into datasets/.
set -euo pipefail
cd "$(dirname "$0")"
ROOT=$(pwd)
. ./pins.env

[ "$(uname -m)" = x86_64 ] && grep -q avx512bw /proc/cpuinfo \
  || { echo "reproduce-avx512.sh runs on an x86_64 host with AVX-512BW" >&2; exit 2; }
SIZE=fast PER_CELL=1
for arg in "$@"; do
  case $arg in
    --small) SIZE=fast PER_CELL=1 ;;
    --medium) SIZE=medium PER_CELL=10 ;;
    --large) SIZE=full PER_CELL=20 ;;
    --min-iters=*) export MIN_ITERS=${arg#--min-iters=} ;;
    *) sed -n '2,18p' "$0"; exit 2 ;;
  esac
done

stage() { echo; echo "===== reproduce: $* ====="; }

stage "sibling repos at their pins"
ONPAIR=$ROOT/../onpair
[ -d "$ONPAIR" ] || { git clone -q "$ONPAIR_URL" "$ONPAIR" && git -C "$ONPAIR" checkout -q "$ONPAIR_SHA"; }
FSST=$ROOT/../duckdb/third_party/fsst/upstream
[ -d "$FSST" ] || FSST=$ROOT/../fsst
[ -d "$FSST" ] || { git clone -q "$FSST_URL" "$FSST" && git -C "$FSST" checkout -q "$FSST_SHA"; }
export ONPAIR FSST

# The AVX-512 kernels are compile-time gated, so the harness is built native.
export RUSTFLAGS="-C target-cpu=native"

stage "build and test"
cargo build --release --workspace
cargo test --release --workspace

stage "the four columns"
[ -x .venv/bin/python ] || { python3 -m venv .venv && .venv/bin/pip install --quiet -r datasets/requirements.txt; }
.venv/bin/python datasets/prepare.py --all

stage "needle suites, $PER_CELL per cell"
./target/release/matcher-selection --config experiments/matcher_selection/config-1mi.toml --per-cell "$PER_CELL" --bless

stage "benchmark blocks ($SIZE)"
paper/gen-micro.sh "$SIZE" avx512
paper/gen-rows.sh
paper/gen-phases.sh "$SIZE" avx512
# Runs the AVX2 sweep with a default build and the AVX-512 one native itself.
env -u RUSTFLAGS paper/reproduce-kernel.sh

uv run paper/check_inputs.py --update || true
