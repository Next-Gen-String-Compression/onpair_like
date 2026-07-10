#!/bin/bash
cd "$(dirname "$0")/.."
until grep -q "REMAINING GROUPS COMPLETE" analysis/run_remaining.log 2>/dev/null; do sleep 20; done
echo "ALL REMAINING COMPLETE"; tail -25 analysis/run_remaining.log
