#!/bin/bash
# Comprehensive sequential benchmark driver. One run at a time (timing hygiene,
# DESIGN.md §9). Logs per-group wall-clock + health. Continues past failures.
set -uo pipefail
cd "$(dirname "$0")/.."
BENCH=target/release/bench
LOG=analysis/run_all.log
: > "$LOG"

run() {  # run <label> <out_dir> <spec>
  local label="$1" out="$2" spec="$3" t0 t1 rc
  echo "[$(date +%T)] START $label -> $out ($spec)" | tee -a "$LOG"
  t0=$(date +%s)
  "$BENCH" run "$spec" -o "$out" >>"$LOG" 2>&1
  rc=$?
  t1=$(date +%s)
  local rows gate err uns
  rows=$(wc -l < "$out/results.jsonl" 2>/dev/null || echo 0)
  gate=$(grep -c '"hash_ok":false' "$out/results.jsonl" 2>/dev/null || echo 0)
  err=$(grep -c '"status":"error"' "$out/results.jsonl" 2>/dev/null || echo 0)
  uns=$(grep -c '"status":"unsupported"' "$out/results.jsonl" 2>/dev/null || echo 0)
  echo "[$(date +%T)] DONE  $label rc=$rc $((t1-t0))s rows=$rows gate_fail=$gate err=$err unsupported=$uns" | tee -a "$LOG"
}

# B. scanner shootout — remaining two columns (msmarco already done via probe)
run shootout-rest results/cx-shootout-rest specs/shootout/tier1-rest.toml

# C. query sweep — all seven columns, uncompressed vs onpair, full grid
for spec in specs/paper/*.toml; do
  ds=$(basename "$spec" .toml)
  run "query-$ds" "results/cx-query/$ds" "$spec"
done

# D. compressed-domain LIKE — fsst family + onpair
run fsstlike results/cx-fsstlike specs/compression/fsst_like_query.toml

# E. unified candidate head-to-head (all query-capable candidates)
run candidates results/cx-candidates specs/shootout/candidates.toml

echo "[$(date +%T)] ALL GROUPS COMPLETE" | tee -a "$LOG"
