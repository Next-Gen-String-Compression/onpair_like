#!/usr/bin/env sh
# Logs the prefilter's alignment graph and min cut for every needle of the
# 2^20-row roster, per 524288-row row group, for OnPair at 12 and 16 bits
# (prune 1.0, as the micro spec runs them) and for the FSST port.
#
#   paper/gen-plan-log.sh [--fast|--medium]
#
# Needs the onpair and fsst checkouts beside this repository (override with
# ONPAIR=... FSST=...); the loggers are onpair's examples/plan_log.rs and the
# fork's search/tools/plan_log.cpp. Writes results/plan-log/<tag>-<date>/ with
# queries.csv, symbols.csv and graph.csv, which tools/graph-viz draws and
# paper/plot_plan.py plots.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT="$HERE/.."
ONPAIR=$(cd "${ONPAIR:-$ROOT/../onpair}" && pwd)
FSST=$(cd "${FSST:-$ROOT/../fsst}" && pwd)
ROW_GROUP=524288

SPEC="$HERE/specs/micro-1mi.toml"
TAG=micro-1mi
case "${1:-}" in
  --fast) SPEC="$HERE/specs/micro-1mi-fast.toml"; TAG=micro-1mi-fast ;;
  --medium) SPEC="$HERE/specs/micro-1mi-medium.toml"; TAG=micro-1mi-medium ;;
esac
OUT="$ROOT/results/plan-log/$TAG-$(date +%F)"
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
