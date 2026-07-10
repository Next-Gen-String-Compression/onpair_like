#!/bin/bash
cd "$(dirname "$0")/.."
# wait for the calibration probe to finish so nothing runs concurrently
until [ -f results/cx-probe/results.jsonl ]; do sleep 5; done
sleep 3
exec bash analysis/run_all.sh
