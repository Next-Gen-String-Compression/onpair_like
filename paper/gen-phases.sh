#!/usr/bin/env sh
# Runs the phase-shares verifiers, specs/phases/verify-<size>.toml, and moves
# paper/config.py onto the new run where it pins an older one of the same name.
# medium-walk is the medium spec cut to the walk at 12 bits, under the medium
# run name.
#
#   paper/gen-phases.sh [fast|medium|medium-walk|full] [neon|avx512]
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT="$HERE/.."
SIZE=${1:-full}
case $SIZE in fast | medium | medium-walk | full) ;; *) echo "unknown size: $SIZE" >&2; exit 2 ;; esac
if [ $# -gt 1 ]; then ARCH=$2; else
  case $(uname -m) in arm64 | aarch64) ARCH=neon ;; *) ARCH=avx512 ;; esac
fi
case $ARCH in neon | avx512) ;; *) echo "unknown instruction set: $ARCH (neon, avx512)" >&2; exit 2 ;; esac

NAME="verify-${SIZE%-walk}-$ARCH-$(date +%F)"
(cd "$ROOT" && cargo build --release --bin bench)
"$ROOT/target/release/bench" run "$HERE/specs/phases/verify-$SIZE.toml" ${MIN_ITERS:+--min-iters "$MIN_ITERS"} -o "$ROOT/results/paper/$NAME"
(cd "$ROOT" && uv run paper/check_inputs.py --update | grep -v '^missing') || true
echo "-> run $NAME"
