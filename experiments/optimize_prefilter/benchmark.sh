#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_dir"

seed=42
profile="prototype"
bench_args=()
while (($#)); do
  case "$1" in
    --seed)
      [[ $# -ge 2 ]] || { echo "--seed requires a value" >&2; exit 2; }
      seed="$2"
      shift 2
      ;;
    --profile)
      [[ $# -ge 2 ]] || { echo "--profile requires a value" >&2; exit 2; }
      profile="$2"
      shift 2
      ;;
    *)
      bench_args+=("$1")
      shift
      ;;
  esac
done
if [[ ! "$profile" =~ ^[[:alnum:]][[:alnum:]_-]*$ ]]; then
  echo "invalid profile name: $profile" >&2
  exit 2
fi
spec="experiments/optimize_prefilter/generated/${profile}/seed-${seed}/benchmark.toml"
results="results/optimize_prefilter/${profile}/seed-${seed}"
explorer="$results/explorer.html"

if [[ ! -f "$spec" ]]; then
  echo "missing $spec; run ./experiments/optimize_prefilter/generate.sh --profile $profile --seed $seed first" >&2
  exit 2
fi

cargo build --release -p lb-harness --bin bench \
  --no-default-features \
  --features cand-uncompressed-memmem,cand-onpair-spiral,scan-memmem
# bash 3.2, which macOS still ships, treats an empty array expansion as an
# unbound variable under `set -u`, so the plain no-argument invocation aborted
# here. The `+` form expands to nothing when the array is empty.
target/release/bench run "$spec" --out "$results" ${bench_args[@]+"${bench_args[@]}"}
python3 tools/bench-viz/bench_viz.py "$results" \
  --show memmem \
  --title "OnPair prefilter experiment ($profile, seed $seed)" \
  --subtitle "Profile $profile; exact selectivity; needle lengths 1-64" \
  --out "$explorer"

echo "explorer: $explorer"

# Open it, unless stdout is redirected (CI, a log file) or the caller opted out.
# The explorer is a single self-contained file, so the browser needs nothing else.
if [[ "${OPEN_EXPLORER:-1}" == "1" && -t 1 ]]; then
  if command -v open >/dev/null 2>&1; then
    open "$explorer"
  elif command -v xdg-open >/dev/null 2>&1; then
    xdg-open "$explorer"
  fi
fi
