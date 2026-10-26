#!/bin/sh
# Keep this candidate's FSST-LIKE code and its calin2110 FSST fork to itself at
# the final harness link, where the `fsst` candidate brings cwida's FSST under
# the same names (libfsst::SymbolTable's shared_ptr, the QSymbol hash table):
# strong globals localized, weak symbols and COMDAT group signatures suffixed
# .lb_fsst_like_tum, as simd_cut_fsst/cpp/localize.sh (which says why), with
# one entry point left global.
#
# The weak COMDAT symbols are renamed rather than localized: a blanket
# `objcopy --keep-global-symbol=<entry>` demotes them to local, which corrupts
# their group sections -- the final linker discards them and intra-object
# references dangle. lld tolerated that for this archive but crashed the
# co-linked `fsst` candidate; bfd refused to link at all. Renamed, they stay
# valid groups. The LLVM symbols this object only references stay as they are.
# See DESIGN.md §17.
set -eu
obj="$1"
tag=lb_fsst_like_tum
nm --defined-only "$obj" \
  | awk '($2=="W" || $2=="V" || $2=="u") { print $3 }' > "$obj.rename"
readelf -gW "$obj" | awk -F'[][]' '/^COMDAT group section/ { print $4 }' >> "$obj.rename"
sort -u "$obj.rename" | awk -v tag="$tag" '{ print $1, $1 "." tag }' > "$obj.redefine"
objcopy --redefine-syms="$obj.redefine" "$obj"
nm --defined-only "$obj" \
  | awk '($2=="T" || $2=="D" || $2=="B" || $2=="R") && $3 != "lb_candidate_fsst_like_tum" { print $3 }' \
  | sort -u > "$obj.localize"
objcopy --localize-symbols="$obj.localize" "$obj"
