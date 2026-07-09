#!/bin/bash
# Reproduce the benchmark end to end, stage by stage.
#
#   ./reproduce.sh <stage>    run one stage
#   ./reproduce.sh all        run every stage in order
#
# Stages (wall-clock on an M-series laptop; network stages depend on link):
#   build      cargo build --release + full test suite            (~3 min)
#   datasets   fetch + extract + ingest the default roster        (hours;
#              ~12 GB of downloads — see `datasets/prepare.py --list`)
#   suites     deterministic query generation: `bench gen --seed 42`
#              + `bench bless` for every materialised dataset     (~5 min each)
#   run        every spec in specs/paper/ -> results/paper/<name> (~10 min each)
#   all        everything above, in order
#
# Reproducibility contract:
#   - datasets/sources.yaml pins raw sha256 + canonical checksums; prepare.py
#     refuses to continue across a mismatch.
#   - suites are pure functions of (dataset checksum, generator version,
#     seed): re-running `gen` reproduces queries.jsonl byte-for-byte, and
#     bless verifies rather than overwrites existing truth.
#   - every run writes manifest.json (spec hash, dataset/suite checksums,
#     environment, module versions) next to its results.jsonl.
#
# Environment control for timing stages: close other workloads; on Linux set
# the performance governor and disable turbo; on macOS neither is
# user-controllable — expect ~5% run-to-run noise and rely on medians.
set -euo pipefail
cd "$(dirname "$0")"

VENV=.venv
PY=$VENV/bin/python
SEED=42

ensure_venv() {
  if [ ! -x "$PY" ]; then
    python3 -m venv "$VENV"
    "$VENV/bin/pip" install --quiet --upgrade pip
    "$VENV/bin/pip" install --quiet -r datasets/requirements.txt
  fi
}

stage_build() {
  cargo build --release
  cargo test --release
}

stage_datasets() {
  ensure_venv
  "$PY" datasets/prepare.py --all
}

stage_suites() {
  local bench=target/release/bench
  for manifest in datasets/*/manifest.json; do
    local dir id suite
    dir=$(dirname "$manifest")
    id=$(basename "$dir")
    case "$id" in mini|fixtures) continue ;; esac  # dev fixtures, not paper data
    suite="suites/${id}-gen1-s${SEED}"
    if [ -f "$suite/queries.jsonl" ]; then
      # Deterministic regeneration check is free; bless verifies stored truth.
      echo "=== $id: suite exists, verifying binding ==="
      "$bench" check --suite "$suite" --dataset "$dir"
    else
      echo "=== $id: generating (seed $SEED) + blessing ==="
      "$bench" gen --dataset "$dir" --out "$suite" --seed "$SEED"
      "$bench" bless --suite "$suite" --dataset "$dir"
    fi
  done
}

stage_run() {
  local bench=target/release/bench
  shopt -s nullglob
  local specs=(specs/paper/*.toml)
  if [ ${#specs[@]} -eq 0 ]; then
    echo "no specs in specs/paper/ — nothing to run"
    return 1
  fi
  for spec in "${specs[@]}"; do
    local name
    name=$(basename "$spec" .toml)
    echo "=== running $spec ==="
    "$bench" run "$spec" -o "results/paper/$name"
  done
}

case "${1:-}" in
  build|datasets|suites|run)
    "stage_$1" ;;
  all)
    for s in build datasets suites run; do
      echo "===== reproduce: $s ====="
      "stage_$s"
    done ;;
  *)
    sed -n '2,27p' "$0"; exit 1 ;;
esac
