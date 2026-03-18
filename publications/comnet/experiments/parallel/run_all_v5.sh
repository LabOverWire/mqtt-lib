#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=============================================="
echo "  V5 Experiment Suite — Full Rerun"
echo "  3 parallel groups, ~21h estimated wall time"
echo "=============================================="
echo ""

echo "=== Phase 1a: HOL blocking v3 (all 3 groups) ==="
echo "  Group 1: tcp, quic-control"
echo "  Group 2: quic-pertopic"
echo "  Group 3: quic-perpub"
echo ""

V3_TRANSPORTS="tcp quic-control" GROUP=1 bash "${SCRIPT_DIR}/02_hol_blocking_v5.sh" \
    > /tmp/v5_phase1a_group1.log 2>&1 &
P1A_PID1=$!
echo "Phase 1a Group 1 started (pid ${P1A_PID1}), log: /tmp/v5_phase1a_group1.log"

V3_TRANSPORTS="quic-pertopic" GROUP=2 bash "${SCRIPT_DIR}/02_hol_blocking_v5.sh" \
    > /tmp/v5_phase1a_group2.log 2>&1 &
P1A_PID2=$!
echo "Phase 1a Group 2 started (pid ${P1A_PID2}), log: /tmp/v5_phase1a_group2.log"

V3_TRANSPORTS="quic-perpub" GROUP=3 bash "${SCRIPT_DIR}/02_hol_blocking_v5.sh" \
    > /tmp/v5_phase1a_group3.log 2>&1 &
P1A_PID3=$!
echo "Phase 1a Group 3 started (pid ${P1A_PID3}), log: /tmp/v5_phase1a_group3.log"

echo ""
echo "Waiting for Phase 1a (HOL v3) to complete..."
echo "  Monitor: tail -f /tmp/v5_phase1a_group{1,2,3}.log"

wait $P1A_PID1 && echo "Phase 1a Group 1 complete" || echo "Phase 1a Group 1 FAILED (exit $?)"
wait $P1A_PID2 && echo "Phase 1a Group 2 complete" || echo "Phase 1a Group 2 FAILED (exit $?)"
wait $P1A_PID3 && echo "Phase 1a Group 3 complete" || echo "Phase 1a Group 3 FAILED (exit $?)"

echo ""
echo "=== Phase 1b + Phase 2: HOL v4 + remaining experiments ==="
echo "  Group 1: HOL v4 pertopic-isolated → Exp 5"
echo "  Group 2: HOL v4 perpub-isolated → Exp 3"
echo "  Group 3: Exp 4 → Exp 6 → Exp 1"
echo ""

bash "${SCRIPT_DIR}/run_group1_v5.sh" > /tmp/v5_group1.log 2>&1 &
G1_PID=$!
echo "Group 1 started (pid ${G1_PID}), log: /tmp/v5_group1.log"

bash "${SCRIPT_DIR}/run_group2_v5.sh" > /tmp/v5_group2.log 2>&1 &
G2_PID=$!
echo "Group 2 started (pid ${G2_PID}), log: /tmp/v5_group2.log"

bash "${SCRIPT_DIR}/run_group3_v5.sh" > /tmp/v5_group3.log 2>&1 &
G3_PID=$!
echo "Group 3 started (pid ${G3_PID}), log: /tmp/v5_group3.log"

echo ""
echo "Waiting for all groups to complete..."
echo "  Monitor: tail -f /tmp/v5_group{1,2,3}.log"

wait $G1_PID && echo "Group 1 complete" || echo "Group 1 FAILED (exit $?)"
wait $G2_PID && echo "Group 2 complete" || echo "Group 2 FAILED (exit $?)"
wait $G3_PID && echo "Group 3 complete" || echo "Group 3 FAILED (exit $?)"

echo ""
echo "=============================================="
echo "  V5 Experiment Suite COMPLETE"
echo "  Results in: experiments/results-v5/"
echo "=============================================="
