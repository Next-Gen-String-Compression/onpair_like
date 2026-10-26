#!/usr/bin/env sh
# Whether this host can draw the figures: latex on PATH and the three packages
# every plot script loads (paperfig.TEX_PREAMBLE). Exits 0 quietly, or names
# what is missing and exits 1.
#
#   sh paper/check-tex.sh
missing=
command -v latex >/dev/null 2>&1 || missing=latex
for sty in libertine.sty zi4.sty newtxmath.sty; do
  kpsewhich "$sty" >/dev/null 2>&1 || missing="$missing $sty"
done
[ -n "$missing" ] || exit 0
echo "TeX for the figures: missing ${missing# } (TeX Live or MacTeX with the CTAN packages libertine, inconsolata and newtx)" >&2
exit 1
