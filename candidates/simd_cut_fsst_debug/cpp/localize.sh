#!/bin/sh
# Localize the FSST symbols in the combined relocatable object so this candidate's
# FSST copy cannot clash with the other candidates' copies at the harness link,
# without touching weak COMDAT symbols; as simd_cut_fsst/cpp/localize.sh, with
# one entry point left global.
set -eu
obj="$1"
list="$obj.localize"
nm --defined-only "$obj" \
  | awk '($2=="T" || $2=="D" || $2=="B") && $3 != "lb_candidate_simd_cut_fsst_debug" { print $3 }' \
  | sort -u > "$list"
objcopy --localize-symbols="$list" "$obj"
