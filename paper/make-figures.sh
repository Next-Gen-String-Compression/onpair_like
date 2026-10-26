#!/usr/bin/env sh
# Redraws every figure the preprint includes and prints Table 1: from the runs
# paper/config.py pins when this tree holds them all (the authors'), checked
# first and stopping on any finding; else from the newest runs this tree holds
# (paper/inputs.py), every figure their machines' runs feed, listing the others
# as skipped.
#
#   sh paper/make-figures.sh [--from DIR] [--pinned | --latest] [--out DIR]
#
# --from DIR  first copies another clone's runs (results/paper, results/plan-log,
#             results/e2e) and kernel sweeps (paper/data), or a copy of its
#             results/ and paper/data/, into this tree and loads them: the
#             Mac's beside the AVX-512 host's, or the other way round
# --pinned, --latest  pick the inputs rather than by which runs are here
# --out DIR   default: the preprint's assets/ with the pinned runs,
#             paper/out/figures/ with the newest ones
set -eu
cd "$(dirname "$0")/.."
MODE= FROM= OUT=
while [ $# -gt 0 ]; do
  case $1 in
    --from) FROM=$2; shift 2 ;;
    --pinned) MODE=pinned; shift ;;
    --latest) MODE=latest; shift ;;
    --out) OUT=$2; shift 2 ;;
    *) sed -n '2,16p' "$0"; exit 2 ;;
  esac
done
# Every plot sets its labels through LaTeX.
sh paper/check-tex.sh
SCRIPTS="plot_fig1 plot_matcher plot_phase_shares plot_micro plot_prefix_suffix plot_selection
  plot_plan_cut plot_kernel_choice plot_e2e plot_e2e_micro plot_no_sort table1"

if [ -n "$FROM" ]; then
  for d in results/paper results/plan-log results/e2e; do
    [ -d "$FROM/$d" ] || continue
    mkdir -p "$d"
    for run in "$FROM/$d"/*; do
      [ -e "$run" ] && [ ! -e "$d/${run##*/}" ] && cp -R "$run" "$d/"
    done
  done
  for f in "$FROM"/paper/data/matcher_*.csv; do
    [ -e "$f" ] && [ ! -e "paper/data/${f##*/}" ] && cp "$f" paper/data/
  done
  [ ! -d results/paper ] || .venv/bin/python tools/results-db/load.py results/paper/*
fi

if [ -z "$MODE" ]; then
  if [ -f results/bench.duckdb ] && ! uv run paper/check_inputs.py 2>/dev/null | grep -q '^missing'; then
    MODE=pinned
  else
    MODE=latest
  fi
fi

if [ "$MODE" = pinned ]; then
  echo "drawing from the runs paper/config.py pins"
  uv run paper/check_inputs.py
  for script in $SCRIPTS; do
    uv run "paper/$script.py" ${OUT:+--out "$OUT"}
  done
  # The panel widths are checked against the preprint's own assets only.
  [ -n "$OUT" ] || uv run paper/check_panels.py
  exit 0
fi

OUT=${OUT:-paper/out/figures}
echo "drawing from the newest runs in this tree into $OUT"
mkdir -p results
export PAPER_INPUTS=results/inputs.json
DRAWABLE=$(uv run paper/inputs.py "$PAPER_INPUTS")
uv run paper/check_inputs.py | grep -v '^missing' || true
FAILED= SKIPPED=
for script in $SCRIPTS; do
  case " $(echo $DRAWABLE) " in
    *" $script "*) uv run "paper/$script.py" --out "$OUT" || FAILED="$FAILED $script" ;;
    *) SKIPPED="$SKIPPED $script" ;;
  esac
done
[ -z "$SKIPPED" ] || echo "skipped, their runs are not in this tree:$SKIPPED"
[ -z "$FAILED" ] || { echo "failed:$FAILED" >&2; exit 1; }
echo "-> $OUT"
