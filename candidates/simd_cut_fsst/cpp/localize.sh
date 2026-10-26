#!/bin/sh
# Keep this candidate's FSST and prefilter code to itself at the final harness
# link, where the other FSST candidates bring their own copies of the same
# names: simd_cut_fsst_debug an older revision of this fork, `fsst` and
# fsst_like_tum other forks. Everything in the combined relocatable object but
# the TWO entry points:
#
#  - strong globals (nm T/D/B/R) are localized; none lies in a COMDAT group;
#  - weak ones (inline functions, template instances, vtables, typeinfo; nm
#    W/V/u) and every COMDAT group signature get the suffix .lb_simd_cut_fsst.
#    Left as they are, the final link keeps one of each same-named group, so
#    one candidate runs the other's code. Localizing them instead corrupts
#    their groups (see fsst_like_tum's localize.sh); renamed, they stay valid
#    groups that no other object shares.
#
# CMakeLists.txt combines the objects with a plain `ld -r`, so any ELF linker
# does; this needs nm, readelf and objcopy. See DESIGN.md §17.
set -eu
obj="$1"
tag=lb_simd_cut_fsst
nm --defined-only "$obj" \
  | awk '($2=="W" || $2=="V" || $2=="u") { print $3 }' > "$obj.rename"
readelf -gW "$obj" | awk -F'[][]' '/^COMDAT group section/ { print $4 }' >> "$obj.rename"
sort -u "$obj.rename" | awk -v tag="$tag" '{ print $1, $1 "." tag }' > "$obj.redefine"
objcopy --redefine-syms="$obj.redefine" "$obj"
nm --defined-only "$obj" \
  | awk '($2=="T" || $2=="D" || $2=="B" || $2=="R") \
         && $3 != "lb_candidate_simd_cut_fsst" \
         && $3 != "lb_candidate_dict_simd_cut_fsst" { print $3 }' \
  | sort -u > "$obj.localize"
objcopy --localize-symbols="$obj.localize" "$obj"
