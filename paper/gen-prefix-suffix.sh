#!/usr/bin/env sh
# Runs PREFIX and SUFFIX on the micro-benchmark's roster,
# specs/prefix-suffix/<size>.toml, on NEON, after generating and blessing any
# suite of that size that is missing, and moves paper/config.py onto the new
# run where it pins an older one of the same name.
#
#   paper/gen-prefix-suffix.sh [fast|medium|full]     default: full
#
# The sizes are micro's: 1, 10 or 20 needles per cell of the same grid.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT="$HERE/.."
SIZE=${1:-full}
case $SIZE in
  fast) PER_CELL=1 ;;
  medium) PER_CELL=10 ;;
  full) PER_CELL=20 ;;
  *) echo "unknown size: $SIZE (fast, medium, full)" >&2; exit 2 ;;
esac

case $(uname -m) in arm64 | aarch64) ARCH=neon ;; *) ARCH=avx512 ;; esac
NAME="prefix-suffix-$SIZE-$ARCH-$(date +%F)"
(cd "$ROOT" && cargo build --release --bin bench && cargo build --release -p matcher-selection)
(cd "$ROOT" && ./target/release/matcher-selection \
  --config experiments/matcher_selection/config-1mi.toml --per-cell "$PER_CELL" --bless)
"$ROOT/target/release/bench" run "$HERE/specs/prefix-suffix/$SIZE.toml" ${MIN_ITERS:+--min-iters "$MIN_ITERS"} -o "$ROOT/results/paper/$NAME"
# The check exits non-zero while an input is still missing or mixed, which is
# not a failed run.
(cd "$ROOT" && uv run paper/check_inputs.py --update) || true
echo "-> run $NAME"
