#!/usr/bin/env sh
# Checks paper/inputs.toml, then redraws every figure the preprint includes
# from the inputs it names. Stops on any check finding.
#
#   paper/make-figures.sh [--out DIR]     default: the preprint's assets/
set -eu
cd "$(dirname "$0")/.."
uv run paper/check_inputs.py
for script in plot_matcher plot_resolver plot_verify_selectivity plot_micro plot_plan; do
  uv run "paper/$script.py" "$@"
done
# The panel widths are checked against the preprint's own assets only.
if [ $# -eq 0 ]; then
  uv run paper/check_panels.py
fi
