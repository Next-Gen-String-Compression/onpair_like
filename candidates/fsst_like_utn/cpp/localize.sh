#!/bin/sh
# Localize the FSST / UTN symbols in the combined relocatable object so this
# candidate's alexandervanrenen FSST fork cannot clash with the `fsst` (cwida)
# and `fsst_like_tum` (calin2110) candidates' FSST symbols at the final harness
# link — WITHOUT touching weak COMDAT symbols. See fsst_like_tum/localize.sh and
# DESIGN.md §17 for the full rationale (localize by binding, not by name, so C++
# template/vtable/typeinfo COMDAT groups stay valid under both lld and bfd).
set -eu
obj="$1"
list="$obj.localize"
nm --defined-only "$obj" \
  | awk '($2=="T" || $2=="D" || $2=="B") && $3 != "lb_candidate_fsst_like_utn" { print $3 }' \
  | sort -u > "$list"
objcopy --localize-symbols="$list" "$obj"
