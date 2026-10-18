#!/usr/bin/env sh
# Checks paper/config.py, then redraws every figure the preprint includes
# from the inputs it names. Stops on any check finding.
#
#   paper/make-figures.sh [--out DIR]     default: the preprint's assets/
set -eu
cd "$(dirname "$0")/.."
uv run paper/check_inputs.py
for script in plot_matcher plot_phase_shares plot_micro plot_prefix_suffix plot_selection plot_plan_cut plot_kernel_choice plot_e2e plot_e2e_micro; do
  uv run "paper/$script.py" "$@"
done
uv run paper/plot_no_sort.py "$@"
# The panel widths are checked against the preprint's own assets only.
if [ $# -eq 0 ]; then
  uv run paper/check_panels.py
fi
