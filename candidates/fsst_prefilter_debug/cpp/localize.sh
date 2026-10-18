#!/bin/sh
# Localize the FSST symbols in the combined relocatable object so this candidate's
# FSST copy cannot clash with the other candidates' copies at the harness link,
# without touching weak COMDAT symbols; as fsst_prefilter/cpp/localize.sh, with
# one entry point left global.
set -eu
obj="$1"
list="$obj.localize"
nm --defined-only "$obj" \
  | awk '($2=="T" || $2=="D" || $2=="B") && $3 != "lb_candidate_fsst_prefilter_debug" { print $3 }' \
  | sort -u > "$list"
objcopy --localize-symbols="$list" "$obj"
