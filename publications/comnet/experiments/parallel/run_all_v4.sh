#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=== HOL blocking v4 (frame packing): launching 2 parallel groups ==="
echo "  Group 1: quic-pertopic-isolated"
echo "  Group 2: quic-perpub-isolated"
echo ""

V4_TRANSPORTS="quic-pertopic-isolated" GROUP=1 bash "${SCRIPT_DIR}/02_hol_blocking_v4.sh" \
    > /tmp/hol_v4_group1.log 2>&1 &
PID1=$!
echo "Group 1 started (pid ${PID1}), log: /tmp/hol_v4_group1.log"

V4_TRANSPORTS="quic-perpub-isolated" GROUP=2 bash "${SCRIPT_DIR}/02_hol_blocking_v4.sh" \
    > /tmp/hol_v4_group2.log 2>&1 &
PID2=$!
echo "Group 2 started (pid ${PID2}), log: /tmp/hol_v4_group2.log"

echo ""
echo "Waiting for all groups to complete..."
echo "  Monitor: tail -f /tmp/hol_v4_group{1,2}.log"

wait $PID1 && echo "Group 1 complete" || echo "Group 1 FAILED (exit $?)"
wait $PID2 && echo "Group 2 complete" || echo "Group 2 FAILED (exit $?)"

echo ""
echo "=== HOL blocking v4 complete ==="
