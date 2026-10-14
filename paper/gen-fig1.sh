#!/usr/bin/env sh
# Runs Figure 1's LZ4, uncompressed, OnPair16 and FSST SIMD cut bars on the M4 Pro,
# specs/fig1/*.toml; the other systems come from the NEON selection run.
#
#   paper/gen-fig1.sh
# The FSST bars need the per-row port, uncommitted in DuckDB's fsst submodule
# until FSST_SHA moves: FSST_SOURCE_DIR=<duckdb>/third_party/fsst/upstream.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT="$HERE/.."
case $(uname -m) in arm64) ;; *) echo "gen-fig1.sh runs on the M4 Pro only" >&2; exit 2 ;; esac

DATE=$(date +%F)
(cd "$ROOT" && cargo build --release --bin bench)
for spec in lz4 uncompressed onpair fsst; do
  "$ROOT/target/release/bench" run "$HERE/specs/fig1/$spec.toml" -o "$ROOT/results/paper/fig1-$spec-neon-$DATE"
  echo "-> run fig1-$spec-neon-$DATE"
done
