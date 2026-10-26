#!/bin/bash
# Reproduces the paper's NEON measurements on arm64, macOS or Linux (the paper's
# is an M4 Pro), end to end from a clone: build and tests, the four
# columns, the needle suites, every block the paper runs on NEON, and the
# end-to-end runs. reproduce-avx512.sh measures the x86 side; with both
# machines' results/ and paper/data in one tree, paper/make-figures.sh draws
# every figure.
#
#   bash reproduce-neon.sh [--small|--medium|--large] [--prefix-suffix] [--clickbench] [--min-iters=N]
#       default: --medium, the needle set the paper draws from; each spec's own min_iters
#
# The size picks the needle set of the micro, phases, ablation and plan-cut
# blocks, the same four columns and grid cells with 1, 10 or 20 needles per
# cell; Figure 1, the row group block and the end-to-end micro block always take
# 1, so that set is generated too. The micro run alone takes about 0.5, 4.5 and
# 8.8 hours. The ablation has no large set and runs medium there.
# --prefix-suffix also runs the PREFIX/SUFFIX block, about 50 minutes, on 1 per cell.
# --clickbench also runs the end-to-end ClickBench queries over the full table,
# which no figure in the paper draws: 14 GB more downloads, about 90 GB more disk.
# --min-iters=N sets every harness run's timed iterations per cell.
#
# Needs a Rust toolchain, a C++ compiler, cmake, python3, uv and the duckdb
# CLI, and for the end-to-end block ninja, flatc and, on Linux, libclang-dev.
# Downloads about 7 GB for the four columns and 2 GB of JOB data for the
# end-to-end runs, into datasets/ and $E2E_DATA (default e2e/data, about 9 GB).
set -euo pipefail
cd "$(dirname "$0")"

case $(uname -m) in arm64 | aarch64) ;; *) echo "reproduce-neon.sh runs on arm64" >&2; exit 2 ;; esac
SIZE=medium PER_CELL=10 ABLATION=medium PREFIX_SUFFIX= CLICKBENCH=
for arg in "$@"; do
  case $arg in
    --small) SIZE=fast PER_CELL=1 ABLATION=fast ;;
    --medium) SIZE=medium PER_CELL=10 ABLATION=medium ;;
    --large) SIZE=full PER_CELL=20 ABLATION=medium ;;
    --prefix-suffix) PREFIX_SUFFIX=1 ;;
    --clickbench) CLICKBENCH=1 ;;
    --min-iters=*) export MIN_ITERS=${arg#--min-iters=} ;;
    *) sed -n '2,25p' "$0"; exit 2 ;;
  esac
done

stage() { echo; echo "===== reproduce: $* ====="; }
sh paper/check-tex.sh || echo "The runs do not need it; paper/make-figures.sh does, afterwards."

stage "build and test"
cargo build --release --workspace
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
sh paper/gen-micro.sh "$SIZE" neon
[ -z "$PREFIX_SUFFIX" ] || sh paper/gen-prefix-suffix.sh fast
sh paper/gen-rows.sh
sh paper/gen-phases.sh "$SIZE" neon
sh paper/gen-ablation.sh "$ABLATION"
sh paper/gen-fig1.sh
sh paper/reproduce-kernel.sh
sh paper/gen-plan-cut.sh "$SIZE"

stage "end to end"
bash e2e/build.sh
E2E_CLICKBENCH=$CLICKBENCH bash e2e/data.sh
python3 e2e/run.py ${CLICKBENCH:+--workloads clickbench,job}
# Figure 15 draws the end-to-end micro block from 1 needle per cell.
python3 e2e/micro.py --spec fast

stage "done"
echo "The numbers are in results/ and paper/data/. With the AVX-512 host's runs, copied"
echo "into a folder here, sh paper/make-figures.sh --from <that folder> draws the figures."
