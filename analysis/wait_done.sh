#!/bin/bash
cd "$(dirname "$0")/.."
until grep -q "ALL GROUPS COMPLETE" analysis/run_all.log 2>/dev/null; do sleep 15; done
echo "COMPLETE"; tail -20 analysis/run_all.log
