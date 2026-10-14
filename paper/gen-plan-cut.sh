#!/usr/bin/env sh
# Logs the prefilter's alignment graph and min cut for every needle of the
# 2^20-row roster, per 524288-row row group, for OnPair at 12 and 16 bits
# (prune 1.0, as the micro spec runs them) and for the FSST port.
#
#   paper/gen-plan-cut.sh [fast|medium|full]     default: full
#
# Needs the onpair checkout beside this repository and the FSST fork as the
# DuckDB checkout beside it holds it, clean at FSST_SHA in pins.env (override with
# ONPAIR=... FSST=...); the
# loggers are onpair's examples/plan_log.rs and the fork's
# search/tools/plan_log.cpp. Writes results/plan-log/micro-<size>-<date>/
# with queries.csv, symbols.csv and graph.csv, which tools/graph-viz draws and
# paper/plot_plan_cut.py and plot_kernel_choice.py plot.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT="$HERE/.."
ONPAIR=$(cd "${ONPAIR:-$ROOT/../onpair}" && pwd)
FSST=$(cd "${FSST:-$ROOT/../duckdb/third_party/fsst/upstream}" && pwd)
. "$ROOT/pins.env"
[ "$(git -C "$FSST" describe --always --dirty --abbrev=40)" = "$FSST_SHA" ] || { echo "$FSST is not clean at FSST_SHA $FSST_SHA" >&2; exit 1; }
ROW_GROUP=524288

SIZE=${1:-full}
case $SIZE in fast | medium | full) ;; *) echo "unknown size: $SIZE (fast, medium, full)" >&2; exit 2 ;; esac
SPEC="$HERE/specs/micro/$SIZE.toml"
OUT="$ROOT/results/plan-log/micro-$SIZE-$(date +%F)"
DUMP=$(mktemp -d)
trap 'rm -rf "$DUMP"' EXIT

(cd "$ONPAIR" && cargo build --release --example plan_log -q)
cmake --build "$FSST/build-search" --target plan_log >/dev/null
uv run "$HERE/plan_log_dump.py" "$SPEC" "$DUMP"
rm -rf "$OUT"

for col in "$DUMP"/*.col; do
  ds=$(basename "$col" .col)
  for bits in 12 16; do
    "$ONPAIR/target/release/examples/plan_log" --col "$col" --needles "$DUMP/$ds.ndl" \
      --dataset "$ds" --chunk-rows $ROW_GROUP --bits $bits --prune 1.0 --out "$OUT"
  done
  "$FSST/build-search/search/plan_log" --col "$col" --needles "$DUMP/$ds.ndl" \
    --dataset "$ds" --chunk-rows $ROW_GROUP --out "$OUT"
done
echo "$OUT"
