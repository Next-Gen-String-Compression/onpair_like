#!/usr/bin/env sh
# Runs every Figure 1 bar on arm64 (the paper's M4 Pro), specs/fig1/*.toml:
# LZ4, uncompressed, OnPair16 and FSST SIMD cut, and FSST decompress and
# automaton (selection.toml).
#
#   paper/gen-fig1.sh
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT="$HERE/.."
case $(uname -m) in arm64 | aarch64) ;; *) echo "gen-fig1.sh runs on arm64 (NEON) only" >&2; exit 2 ;; esac

DATE=$(date +%F)
(cd "$ROOT" && cargo build --release --bin bench)
for spec in lz4 uncompressed onpair fsst selection; do
  "$ROOT/target/release/bench" run "$HERE/specs/fig1/$spec.toml" ${MIN_ITERS:+--min-iters "$MIN_ITERS"} -o "$ROOT/results/paper/fig1-$spec-neon-$DATE"
  echo "-> run fig1-$spec-neon-$DATE"
done
(cd "$ROOT" && uv run paper/check_inputs.py --update | grep -v '^missing') || true
