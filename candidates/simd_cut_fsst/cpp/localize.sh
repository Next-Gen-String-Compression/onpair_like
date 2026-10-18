#!/bin/sh
# Localize the cwida FSST symbols in the combined relocatable object so this
# candidate's cwida FSST copy cannot clash with the `fsst` / fsst_decode_prefilter
# copies at the final harness link — WITHOUT touching weak COMDAT symbols. Same
# strategy as fsst_like_tum's localize.sh (localize by binding T/D/B, not by
# name); here TWO entry points stay global. See DESIGN.md §17.
set -eu
obj="$1"
list="$obj.localize"
nm --defined-only "$obj" \
  | awk '($2=="T" || $2=="D" || $2=="B") \
         && $3 != "lb_candidate_simd_cut_fsst" \
         && $3 != "lb_candidate_dict_simd_cut_fsst" { print $3 }' \
  | sort -u > "$list"
objcopy --localize-symbols="$list" "$obj"
