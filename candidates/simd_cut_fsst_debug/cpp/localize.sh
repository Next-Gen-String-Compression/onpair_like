#!/bin/sh
# Localize the FSST symbols in the combined relocatable object so this candidate's
# FSST copy cannot clash with the other candidates' copies at the harness link,
# weak symbols included; as simd_cut_fsst/cpp/localize.sh (which says why), with
# one entry point left global.
set -eu
obj="$1"
list="$obj.localize"
nm --defined-only "$obj" \
  | awk '($2=="T" || $2=="D" || $2=="B" || $2=="W" || $2=="V" || $2=="u" || $2=="R") && $3 != "lb_candidate_simd_cut_fsst_debug" { print $3 }' \
  | sort -u > "$list"
objcopy --localize-symbols="$list" "$obj"
