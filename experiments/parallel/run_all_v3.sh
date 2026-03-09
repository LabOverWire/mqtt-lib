#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=== HOL blocking v3: launching 3 parallel groups ==="
echo "  Group 1: tcp, quic-control"
echo "  Group 2: quic-pertopic"
echo "  Group 3: quic-perpub"
echo ""

V3_TRANSPORTS="tcp quic-control" GROUP=1 bash "${SCRIPT_DIR}/02_hol_blocking_v3.sh" \
    > /tmp/hol_v3_group1.log 2>&1 &
PID1=$!
echo "Group 1 started (pid ${PID1}), log: /tmp/hol_v3_group1.log"

V3_TRANSPORTS="quic-pertopic" GROUP=2 bash "${SCRIPT_DIR}/02_hol_blocking_v3.sh" \
    > /tmp/hol_v3_group2.log 2>&1 &
PID2=$!
echo "Group 2 started (pid ${PID2}), log: /tmp/hol_v3_group2.log"

V3_TRANSPORTS="quic-perpub" GROUP=3 bash "${SCRIPT_DIR}/02_hol_blocking_v3.sh" \
    > /tmp/hol_v3_group3.log 2>&1 &
PID3=$!
echo "Group 3 started (pid ${PID3}), log: /tmp/hol_v3_group3.log"

echo ""
echo "Waiting for all groups to complete..."
echo "  Monitor: tail -f /tmp/hol_v3_group{1,2,3}.log"

wait $PID1 && echo "Group 1 complete" || echo "Group 1 FAILED (exit $?)"
wait $PID2 && echo "Group 2 complete" || echo "Group 2 FAILED (exit $?)"
wait $PID3 && echo "Group 3 complete" || echo "Group 3 FAILED (exit $?)"

echo ""
echo "=== HOL blocking v3 complete ==="
