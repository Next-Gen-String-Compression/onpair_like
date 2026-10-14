#!/bin/bash
# Clones the pinned DuckDB fork and duckdb-vortex into e2e/builds and builds
# the three binaries the end-to-end benchmark runs, into e2e/builds/bin:
#
#   duckdb                 the fork (both DuckDB arms)
#   duckdb-vortex-0.86.1   duckdb-vortex with vortex-duckdb 0.86.1 (fork, segment cache)
#   duckdb-vortex-onpair   duckdb-vortex with the OnPair revision (fork, segment cache)
#
#   e2e/build.sh
#
# Revisions are in pins.env at the repository root. A checkout already at its pin is reused, so a
# re-run only rebuilds.
set -euo pipefail
E2E=$(cd "$(dirname "$0")" && pwd)
source "$E2E/../pins.env"
BUILDS=$E2E/builds
BIN=$BUILDS/bin
mkdir -p "$BIN"

# fetch <url> <sha> <dir>: a shallow checkout of one commit and its submodules
fetch() {
  local url=$1 sha=$2 dir=$3
  if [ "$(git -C "$dir" rev-parse HEAD 2>/dev/null)" = "$sha" ]; then
    echo "== $dir at $sha"
  else
    echo "== fetch $url $sha"
    rm -rf "$dir"
    git init -q "$dir"
    git -C "$dir" remote add origin "$url"
    git -C "$dir" fetch -q --depth 1 origin "$sha"
    git -C "$dir" checkout -q FETCH_HEAD
  fi
  git -C "$dir" submodule update -q --init --recursive --depth 1
}

fetch "$DUCKDB_URL" "$DUCKDB_SHA" "$BUILDS/duckdb"
# its fsst submodule has to be FSST_SHA, the fsst every other user in this repository takes
if [ "$(git -C "$BUILDS/duckdb" rev-parse HEAD:third_party/fsst/upstream)" != "$FSST_SHA" ]; then
  echo "DuckDB $DUCKDB_SHA has fsst $(git -C "$BUILDS/duckdb" rev-parse HEAD:third_party/fsst/upstream), not FSST_SHA $FSST_SHA" >&2
  exit 1
fi
cmake -G Ninja -DCMAKE_BUILD_TYPE=Release -DEXTENSION_STATIC_BUILD=1 -DOVERRIDE_GIT_DESCRIBE=$DUCKDB_DESCRIBE \
  -S "$BUILDS/duckdb" -B "$BUILDS/duckdb/build/release"
cmake --build "$BUILDS/duckdb/build/release" --target shell
cp "$BUILDS/duckdb/build/release/duckdb" "$BIN/duckdb"

DV=$BUILDS/duckdb-vortex
fetch "$DUCKDB_VORTEX_URL" "$DUCKDB_VORTEX_SHA" "$DV"
export CARGO_BUILD_JOBS=4 CMAKE_BUILD_PARALLEL_LEVEL=4 GEN=ninja OVERRIDE_GIT_DESCRIBE=$DUCKDB_VORTEX_DESCRIBE
git -C "$DV" checkout -q -- vortex-extension/Cargo.toml vortex-extension/Cargo.lock
grep -q "rev = \"$VORTEX_UPSTREAM_REV\"" "$DV/vortex-extension/Cargo.toml"

# use_vortex <rev> <lock>: point vortex-duckdb at the fork's <rev>, with <lock>
# resolved for its upstream revision rewritten to match
use_vortex() {
  local rev=$1 lock=$2 from
  from=$(grep -o 'git+https://github.com/vortex-data/vortex.git?rev=[0-9a-f]*' "$lock" | head -1 | cut -d= -f2)
  sed -i '' "s|git = \"https://github.com/vortex-data/vortex.git\", rev = \"$VORTEX_UPSTREAM_REV\"|git = \"$VORTEX_URL\", rev = \"$rev\"|" \
    "$DV/vortex-extension/Cargo.toml"
  sed "s|git+https://github.com/vortex-data/vortex.git?rev=$from#$from|git+$VORTEX_URL?rev=$rev#$rev|" "$lock" \
    > "$DV/vortex-extension/Cargo.lock.new"
  mv "$DV/vortex-extension/Cargo.lock.new" "$DV/vortex-extension/Cargo.lock"
  grep -q "rev = \"$rev\"" "$DV/vortex-extension/Cargo.toml"
}

use_vortex "$VORTEX_BASE_REV" "$DV/vortex-extension/Cargo.lock"
make -C "$DV" release
cp "$DV/build/release/duckdb" "$BIN/duckdb-vortex-0.86.1"
git -C "$DV" checkout -q -- vortex-extension/Cargo.toml vortex-extension/Cargo.lock

# The OnPair build takes the lock file resolved for its revision.
use_vortex "$VORTEX_ONPAIR_REV" "$E2E/vortex-onpair.Cargo.lock"
make -C "$DV" release EXT_FLAGS="-DCMAKE_OSX_DEPLOYMENT_TARGET=12.0 -DVORTEX_VERSION=onpair-${VORTEX_ONPAIR_REV:0:7}"
cp "$DV/build/release/duckdb" "$BIN/duckdb-vortex-onpair"
git -C "$DV" checkout -q -- vortex-extension/Cargo.toml vortex-extension/Cargo.lock

for b in duckdb duckdb-vortex-0.86.1 duckdb-vortex-onpair; do
  echo "$b: $("$BIN/$b" -noheader -list -c "SELECT version()")"
done
