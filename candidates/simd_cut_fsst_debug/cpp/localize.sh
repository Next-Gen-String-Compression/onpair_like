#!/bin/sh
# Keep this candidate's FSST and prefilter code to itself at the final harness
# link: strong globals localized, weak symbols and COMDAT group signatures
# suffixed .lb_simd_cut_fsst_debug; as simd_cut_fsst/cpp/localize.sh (which
# says why), with one entry point left global.
set -eu
obj="$1"
tag=lb_simd_cut_fsst_debug
nm --defined-only "$obj" \
  | awk '($2=="W" || $2=="V" || $2=="u") { print $3 }' > "$obj.rename"
readelf -gW "$obj" | awk -F'[][]' '/^COMDAT group section/ { print $4 }' >> "$obj.rename"
sort -u "$obj.rename" | awk -v tag="$tag" '{ print $1, $1 "." tag }' > "$obj.redefine"
objcopy --redefine-syms="$obj.redefine" "$obj"
nm --defined-only "$obj" \
  | awk '($2=="T" || $2=="D" || $2=="B" || $2=="R") && $3 != "lb_candidate_simd_cut_fsst_debug" { print $3 }' \
  | sort -u > "$obj.localize"
objcopy --localize-symbols="$obj.localize" "$obj"
