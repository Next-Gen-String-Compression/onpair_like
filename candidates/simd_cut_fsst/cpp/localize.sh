#!/bin/sh
# Localize the cwida FSST symbols in the combined relocatable object so this
# candidate's cwida FSST copy cannot clash with the `fsst` / fsst_decode_prefilter
# copies at the final harness link: every defined global by binding, not by name,
# but the TWO entry points. Unlike fsst_like_tum's localize.sh this includes the
# weak symbols (inline functions, template instances, vtables, typeinfo; nm
# W/V/u). Left global, the final link folds them with simd_cut_fsst_debug's
# same-named copies of an older revision of the fork, and one candidate runs the
# other's code. Localizing them is safe only because CMakeLists.txt combines the
# objects with `ld -r --force-group-allocation`, which turns COMDAT group members
# into plain sections: localizing a symbol of a live COMDAT group corrupts the
# group (see fsst_like_tum's localize.sh). See DESIGN.md §17.
set -eu
obj="$1"
list="$obj.localize"
nm --defined-only "$obj" \
  | awk '($2=="T" || $2=="D" || $2=="B" || $2=="W" || $2=="V" || $2=="u" || $2=="R") \
         && $3 != "lb_candidate_simd_cut_fsst" \
         && $3 != "lb_candidate_dict_simd_cut_fsst" { print $3 }' \
  | sort -u > "$list"
objcopy --localize-symbols="$list" "$obj"
