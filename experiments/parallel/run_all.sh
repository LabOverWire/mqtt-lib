#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "launching 3 experiment groups in parallel..."
echo "  group 1: exp 02 (HOL blocking, 15 runs)"
echo "  group 2: exp 01, 03, 05"
echo "  group 3: exp 04, 06"
echo ""
echo "monitor progress:"
echo "  tail -f ${SCRIPT_DIR}/group1.log"
echo "  tail -f ${SCRIPT_DIR}/group2.log"
echo "  tail -f ${SCRIPT_DIR}/group3.log"
echo ""

bash "${SCRIPT_DIR}/run_group1.sh" &
PID1=$!
bash "${SCRIPT_DIR}/run_group2.sh" &
PID2=$!
bash "${SCRIPT_DIR}/run_group3.sh" &
PID3=$!

echo "PIDs: group1=${PID1} group2=${PID2} group3=${PID3}"
echo "waiting for all groups to finish..."

FAILED=0
wait $PID1 || { echo "group 1 FAILED"; FAILED=1; }
wait $PID2 || { echo "group 2 FAILED"; FAILED=1; }
wait $PID3 || { echo "group 3 FAILED"; FAILED=1; }

if [ "$FAILED" -eq 0 ]; then
    echo "all experiments complete"
else
    echo "some groups failed - check logs"
    exit 1
fi
