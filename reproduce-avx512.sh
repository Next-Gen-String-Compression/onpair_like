#!/bin/bash
# Reproduces the paper's x86 measurements on an AVX-512 Linux host (the EPYC
# 9R14 of a c7a.xlarge), end to end from a clone: build and tests, the four
# columns, the needle suites, and every block the paper runs on AVX-512, with
# the kernel sweep on AVX2 as well.
# reproduce-neon.sh measures the arm64 side; with both machines' results/ and
# paper/data in one tree, paper/make-figures.sh draws every figure.
#
#   bash reproduce-avx512.sh [--small|--medium|--large] [--min-iters=N]
#       default: --medium, the needle set the paper draws from; each spec's own min_iters
#
# The size picks the needle set of the micro and phases blocks, the same four
# columns and grid cells with 1, 10 or 20 needles per cell; the row group block
# always takes 1, so that set is generated too. The micro run alone takes about
# 0.6, 5.3 and 10.4 hours. --min-iters=N sets every harness run's timed
# iterations per cell. Draw the figures with paper/make-figures.sh (README.md).
#
# Needs a Rust toolchain, a C++ compiler, cmake, python3, uv, the duckdb CLI,
# and LLVM 14-16 for the `llvm-simd` baseline. Downloads about 7 GB for the
# four columns into datasets/.
set -euo pipefail
cd "$(dirname "$0")"

[ "$(uname -m)" = x86_64 ] && grep -q avx512bw /proc/cpuinfo \
  || { echo "reproduce-avx512.sh needs an x86_64 host with AVX-512BW, such as an AWS c7a.xlarge (README.md, Requirements)" >&2; exit 2; }
SIZE=medium PER_CELL=10
for arg in "$@"; do
  case $arg in
    --small) SIZE=fast PER_CELL=1 ;;
    --medium) SIZE=medium PER_CELL=10 ;;
    --large) SIZE=full PER_CELL=20 ;;
    --min-iters=*) export MIN_ITERS=${arg#--min-iters=} ;;
    *) sed -n '2,20p' "$0"; exit 2 ;;
  esac
done

stage() { echo; echo "===== reproduce: $* ====="; }

# The AVX-512 kernels are compile-time gated, so the harness is built native.
export RUSTFLAGS="-C target-cpu=native"

# LLVM 14-16 for fsst_like_tum's compiled backends (llvm, llvm-simd), which distributions after 2024
# no longer package: an installed one, else LLVM's 16.0.4 release in target/llvm-16, whose static
# libraries link zstd, libxml2 and libtinfo: without their -dev packages fsst_like_tum leaves
# the backends out. Optional: without LLVM those backends are skipped.
stage "LLVM 16 (optional)"
if [ -z "${LLVM_ROOT:-}" ]; then
  for v in 16 15 14; do
    [ -d /usr/lib/llvm-$v/lib/cmake/llvm ] && { export LLVM_ROOT=/usr/lib/llvm-$v; break; }
  done
fi
if [ -z "${LLVM_ROOT:-}" ]; then
  LLVM=$PWD/target/llvm-16
  if [ -x "$LLVM/bin/llvm-config" ] || { mkdir -p "$LLVM" && curl -fsSL \
      https://github.com/llvm/llvm-project/releases/download/llvmorg-16.0.4/clang+llvm-16.0.4-x86_64-linux-gnu-ubuntu-22.04.tar.xz \
      | tar -xJ -C "$LLVM" --strip-components=1; }; then
    export LLVM_ROOT=$LLVM
  else
    rm -rf "$LLVM"
  fi
fi
if [ -n "${LLVM_ROOT:-}" ]; then
  for lib in zstd xml2 tinfo; do
    ldconfig -p | grep "lib$lib\.so " >/dev/null && continue
    sudo -n apt-get install -y -q libzstd-dev libxml2-dev libncurses-dev >/dev/null 2>&1 || true
    break
  done
fi
echo "LLVM: ${LLVM_ROOT:-none, the llvm and llvm-simd backends are skipped}"

stage "build and test"
cargo build --release --workspace
if [ -n "${LLVM_ROOT:-}" ] && ! ./target/release/bench list | grep '"llvm-simd"' >/dev/null; then
  echo "warning: LLVM is at $LLVM_ROOT but the llvm-simd baseline was not built: install" \
    "libzstd-dev libxml2-dev libncurses-dev, run cargo clean --release -p lb-cand-fsst-like-tum" \
    "and run this script again, or its series stay empty" >&2
fi
cargo test --release --workspace

stage "the four columns"
[ -x .venv/bin/python ] || { python3 -m venv .venv && .venv/bin/pip install --quiet -r datasets/requirements.txt; }
.venv/bin/python datasets/prepare.py --all

stage "needle suites, 1 and $PER_CELL per cell"
for n in $(printf '%s\n' 1 "$PER_CELL" | sort -un); do
  ./target/release/matcher-selection --config experiments/matcher_selection/config-1mi.toml --per-cell "$n" --bless
done

stage "benchmark blocks ($SIZE)"
# Every script runs through its interpreter: the anonymized download drops the
# executable bits.
sh paper/gen-micro.sh "$SIZE" avx512
sh paper/gen-rows.sh
sh paper/gen-phases.sh "$SIZE" avx512
# Runs the AVX2 sweep with a default build and the AVX-512 one native itself.
env -u RUSTFLAGS sh paper/reproduce-kernel.sh

stage "done"
echo "The numbers are in results/ and paper/data/. With the Mac's runs, copied into"
echo "a folder here, sh paper/make-figures.sh --from <that folder> draws the figures."
