#!/bin/sh
# Localize the FSST / FSST-LIKE symbols in the combined relocatable object so
# this candidate's calin2110 FSST fork cannot clash with the `fsst` /
# `fsst_like_tum` candidates' FSST symbols at the final harness link — WITHOUT
# touching weak COMDAT symbols. Identical strategy to fsst_like_tum's
# localize.sh; only the exported entry point differs. See that file and
# DESIGN.md §17 for the full rationale (localize by binding T/D/B, not by name,
# to keep weak COMDAT groups valid under both lld and bfd).
set -eu
obj="$1"
list="$obj.localize"
nm --defined-only "$obj" \
  | awk '($2=="T" || $2=="D" || $2=="B") && $3 != "lb_candidate_dict_fsst_like_tum" { print $3 }' \
  | sort -u > "$list"
objcopy --localize-symbols="$list" "$obj"
