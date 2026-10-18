#!/usr/bin/env bash
# Regenerates the plan-log fixtures the tests read, from the two corpora below,
# with onpair's examples/plan_log.rs (bits 16) and the plan_log of the FSST fork
# in third_party/fsst.
#   tests/fixtures/make.sh
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
onpair="$(cd "$here/../../../../third_party/onpair" && pwd)"
fsst="$(cd "$here/../../../../third_party/fsst" && pwd)"
build="$here/../../../../target/fsst-search"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

python3 - "$tmp" <<'EOF'
import struct, sys
out = sys.argv[1]
plain = [
    "https://www.example.com/index.html",
    "https://www.example.com/search?q=onpair",
    "https://www.example.com/search?q=compression",
    "https://shop.example.org/cart/checkout?ref=newsletter",
    "https://shop.example.org/products/list?page=2&sort=desc",
    "https://news.site.co.uk/articles/2026/08?utm_source=newsletter",
    "https://news.site.co.uk/articles/2026/08?utm_source=google",
    "https://cdn.assets.example.net/static/app.js",
    "https://forum.example.com/user/profile?id=42",
    "https://forum.example.com/search?q=substring-search&page=3",
]
repetitive = [
    f"https://shop.example.org/cart/checkout?ref=newsletter&id={r}" if r % 2 == 0
    else f"https://shop.example.org/cart/basket?ref=google&id={r}"
    for r in range(200)
]
needles = {
    "plain": [b"utm_source=newsletter", b"example", b"?q=", b"co", b"zzz-absent",
              b"/articles/2026/08?utm_source=", b"example.com", b"sho\x0ep?q="],
    "repetitive": [b"newsletter", b"art/checkou", b"ut?ref=newsl", b"rg/cart/chec",
                   b"ref=new", b"/cart/checkout?ref=newsletter"],
}
for name, rows in (("plain", plain), ("repetitive", repetitive)):
    data = [r.encode() for r in rows]
    offsets = [0]
    for d in data:
        offsets.append(offsets[-1] + len(d))
    with open(f"{out}/{name}.col", "wb") as f:
        f.write(struct.pack("<QQ", len(data), offsets[-1]))
        f.write(struct.pack(f"<{len(offsets)}Q", *offsets))
        f.write(b"".join(data))
    with open(f"{out}/{name}.ndl", "wb") as f:
        f.write(struct.pack("<Q", len(needles[name])))
        for n in needles[name]:
            f.write(struct.pack("<Q", len(n)) + n)
EOF

(cd "$onpair" && cargo build --release --example plan_log -q)
cmake -S "$fsst" -B "$build" -DCMAKE_BUILD_TYPE=Release >/dev/null
cmake --build "$build" --target plan_log >/dev/null
for name in plain repetitive; do
    rm -rf "$here/$name"
    "$onpair/target/release/examples/plan_log" --col "$tmp/$name.col" --needles "$tmp/$name.ndl" \
        --dataset "$name" --bits 16 --out "$here/$name" 2>/dev/null
    "$build/search/plan_log" --col "$tmp/$name.col" --needles "$tmp/$name.ndl" \
        --dataset "$name" --out "$here/$name" 2>/dev/null
done
