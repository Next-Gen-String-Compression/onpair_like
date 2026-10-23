#!/bin/bash
# Builds the three binaries the end-to-end benchmark runs, into e2e/builds/bin:
#
#   duckdb                 our DuckDB fork, third_party/duckdb (both DuckDB arms)
#   duckdb-vortex-0.86.1   duckdb-vortex with vortex-duckdb from third_party/vortex-0.86.1
#   duckdb-vortex-onpair   duckdb-vortex with vortex-duckdb from third_party/vortex-onpair
#
#   e2e/build.sh
#
# duckdb-vortex is fetched at its pin in pins.env; a checkout already at it is reused, so a re-run
# only rebuilds.
set -euo pipefail
E2E=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$E2E/.." && pwd)
source "$ROOT/pins.env"
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

# The folder ships without test/, so the unit test runner is off.
cmake -G Ninja -DCMAKE_BUILD_TYPE=Release -DEXTENSION_STATIC_BUILD=1 -DBUILD_UNITTESTS=OFF \
  -DOVERRIDE_GIT_DESCRIBE=$DUCKDB_DESCRIBE -S "$ROOT/third_party/duckdb" -B "$BUILDS/duckdb-release"
cmake --build "$BUILDS/duckdb-release" --target shell
cp "$BUILDS/duckdb-release/duckdb" "$BIN/duckdb"

DV=$BUILDS/duckdb-vortex
fetch "$DUCKDB_VORTEX_URL" "$DUCKDB_VORTEX_SHA" "$DV"
export CARGO_BUILD_JOBS=4 CMAKE_BUILD_PARALLEL_LEVEL=4 GEN=ninja OVERRIDE_GIT_DESCRIBE=$DUCKDB_VORTEX_DESCRIBE
git -C "$DV" checkout -q -- vortex-extension/Cargo.toml vortex-extension/Cargo.lock
grep -q "rev = \"$VORTEX_UPSTREAM_REV\"" "$DV/vortex-extension/Cargo.toml"

# use_vortex <folder> <lock>: take vortex-duckdb from third_party/<folder>, with <lock> resolved for
# its upstream revision; the Vortex packages lose their git source, as path packages carry none.
use_vortex() {
  local dir=$ROOT/third_party/$1 lock=$2
  sed -i.bak "s|git = \"https://github.com/vortex-data/vortex.git\", rev = \"$VORTEX_UPSTREAM_REV\"|path = \"$dir/vortex-duckdb\"|" \
    "$DV/vortex-extension/Cargo.toml"
  grep -v '^source = "git+https://github.com/vortex-data/vortex.git?rev=' "$lock" > "$DV/vortex-extension/Cargo.lock.new"
  mv "$DV/vortex-extension/Cargo.lock.new" "$DV/vortex-extension/Cargo.lock"
  rm -f "$DV/vortex-extension/Cargo.toml.bak"
  grep -q "path = \"$dir/vortex-duckdb\"" "$DV/vortex-extension/Cargo.toml"
}

use_vortex vortex-0.86.1 "$DV/vortex-extension/Cargo.lock"
# The version is given: with vortex-duckdb taken by path, duckdb-vortex's CMake finds no git
# revision to read it from. 0.86.1 is VORTEX_UPSTREAM_REV's tag; the deployment target is
# the Makefile's default EXT_FLAGS, which this replaces.
make -C "$DV" release EXT_FLAGS="-DCMAKE_OSX_DEPLOYMENT_TARGET=12.0 -DVORTEX_VERSION=0.86.1"
cp "$DV/build/release/duckdb" "$BIN/duckdb-vortex-0.86.1"
git -C "$DV" checkout -q -- vortex-extension/Cargo.toml vortex-extension/Cargo.lock

# The OnPair build takes the lock file resolved for its revision.
use_vortex vortex-onpair "$E2E/vortex-onpair.Cargo.lock"
make -C "$DV" release EXT_FLAGS="-DCMAKE_OSX_DEPLOYMENT_TARGET=12.0 -DVORTEX_VERSION=onpair"
cp "$DV/build/release/duckdb" "$BIN/duckdb-vortex-onpair"
git -C "$DV" checkout -q -- vortex-extension/Cargo.toml vortex-extension/Cargo.lock

for b in duckdb duckdb-vortex-0.86.1 duckdb-vortex-onpair; do
  echo "$b: $("$BIN/$b" -noheader -list -c "SELECT version()")"
done
