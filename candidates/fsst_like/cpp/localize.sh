#!/bin/sh
# Localize the FSST / FSST-LIKE symbols in the combined relocatable object so
# this candidate's calin2110 FSST fork cannot clash with the `fsst` candidate's
# cwida FSST symbols at the final harness link — WITHOUT touching weak COMDAT
# symbols.
#
# We localize only STRONG-global defined symbols (nm types T/D/B), excluding the
# single entry point. Those are the real hard clashers (duplicate strong defs),
# and being non-COMDAT they localize cleanly. Weak COMDAT symbols (C++ template
# instantiations, vtables, typeinfo, inline fsst_decompress, libstdc++ shared_ptr
# machinery) are LEFT ALONE: a blanket `objcopy --keep-global-symbol=<entry>`
# demotes those to local, which corrupts their group sections — the final linker
# discards them and intra-object references dangle. lld tolerates that for this
# archive but crashes the co-linked `fsst` candidate; bfd refuses to link at all.
# Localizing by binding (not by name) keeps the weak COMDAT groups valid, so the
# result links cleanly under both lld and bfd. See DESIGN.md §17.
set -eu
obj="$1"
list="$obj.localize"
nm --defined-only "$obj" \
  | awk '($2=="T" || $2=="D" || $2=="B") && $3 != "lb_candidate_fsst_like" { print $3 }' \
  | sort -u > "$list"
objcopy --localize-symbols="$list" "$obj"
