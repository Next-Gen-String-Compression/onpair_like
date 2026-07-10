#!/bin/bash
cd "$(dirname "$0")/.."
# wait for Group B (dbpedia shootout) to write its results
until [ -f results/cx-shootout-rest/results.jsonl ]; do sleep 10; done
sleep 5   # let the bench process fully flush/exit before starting the next run
exec bash analysis/run_remaining.sh
