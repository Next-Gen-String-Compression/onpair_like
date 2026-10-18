#!/bin/bash
# Reproduces the paper's NEON measurements on an arm64 Mac (the M4 Pro), end to
# end from a clone: build and tests, the four
# columns, the needle suites, every block the paper runs on NEON, and the
# end-to-end runs. reproduce-avx512.sh measures the x86 side; with both
# machines' results/ and paper/data in one tree, paper/make-figures.sh draws
# every figure.
#
#   ./reproduce-neon.sh [--small|--medium|--large] [--prefix-suffix] [--min-iters=N]
#       default: --small, each spec's own min_iters
#
# The size picks the needle set, the same four columns and grid cells with
# 1, 10 or 20 needles per cell. The micro run alone takes about 0.5, 4.5 and
# 8.8 hours. The ablation has no large set and runs medium there.
# --prefix-suffix also runs the PREFIX/SUFFIX block, about 50 minutes at --small.
# --min-iters=N sets every harness run's timed iterations per cell.
#
# Needs a Rust toolchain, a C++ compiler, cmake, python3, uv and the duckdb
# CLI. Downloads about 7 GB for the four columns and 15 GB for the end-to-end
# data, into datasets/ and $E2E_DATA (default e2e/data).
set -euo pipefail
cd "$(dirname "$0")"

case $(uname -m) in arm64 | aarch64) ;; *) echo "reproduce-neon.sh runs on arm64" >&2; exit 2 ;; esac
SIZE=fast PER_CELL=1 ABLATION=fast PREFIX_SUFFIX=
for arg in "$@"; do
  case $arg in
    --small) SIZE=fast PER_CELL=1 ABLATION=fast ;;
    --medium) SIZE=medium PER_CELL=10 ABLATION=medium ;;
    --large) SIZE=full PER_CELL=20 ABLATION=medium ;;
    --prefix-suffix) PREFIX_SUFFIX=1 ;;
    --min-iters=*) export MIN_ITERS=${arg#--min-iters=} ;;
    *) sed -n '2,20p' "$0"; exit 2 ;;
  esac
done

stage() { echo; echo "===== reproduce: $* ====="; }

stage "build and test"
cargo build --release --workspace
cargo test --release --workspace

stage "the four columns"
[ -x .venv/bin/python ] || { python3 -m venv .venv && .venv/bin/pip install --quiet -r datasets/requirements.txt; }
.venv/bin/python datasets/prepare.py --all

stage "needle suites, $PER_CELL per cell"
./target/release/matcher-selection --config experiments/matcher_selection/config-1mi.toml --per-cell "$PER_CELL" --bless

stage "benchmark blocks ($SIZE)"
paper/gen-micro.sh "$SIZE" neon
[ -z "$PREFIX_SUFFIX" ] || paper/gen-prefix-suffix.sh "$SIZE"
paper/gen-rows.sh
paper/gen-phases.sh "$SIZE" neon
paper/gen-ablation.sh "$ABLATION"
paper/gen-fig1.sh
paper/reproduce-kernel.sh
paper/gen-plan-cut.sh "$SIZE"

stage "end to end"
e2e/build.sh
e2e/data.sh
python3 e2e/run.py
python3 e2e/micro.py --spec "$SIZE"

uv run paper/check_inputs.py --update || true
