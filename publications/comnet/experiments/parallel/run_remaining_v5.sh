#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

: "${GROUP:=1}"

echo "=============================================="
echo "  V5 Remaining Experiments (02b + 02d + 02e)"
echo "  Running on group ${GROUP}"
echo "=============================================="
echo ""

echo "=== Exp 02b: HOL RTT sweep (10/50/100ms, 1% loss) ==="
echo "  4 transports × 3 delays × 15 runs = 180 datapoints"
echo "  Estimated: ~4h"
echo ""
GROUP=${GROUP} bash "${SCRIPT_DIR}/02b_hol_rtt_sweep_v5.sh"
echo ""

echo "=== Exp 02d: HOL RTT boundary (15/20ms, 1% loss) ==="
echo "  4 transports × 2 delays × 15 runs = 120 datapoints"
echo "  Estimated: ~2.5h"
echo ""
GROUP=${GROUP} bash "${SCRIPT_DIR}/02d_hol_rtt_boundary_v5.sh"
echo ""

echo "=== Exp 02e: HOL QoS 1 (25ms, 1% loss) ==="
echo "  2 transports × 15 runs = 30 datapoints"
echo "  Estimated: ~45min"
echo ""
GROUP=${GROUP} bash "${SCRIPT_DIR}/02e_hol_qos1_v5.sh"
echo ""

echo "=============================================="
echo "  V5 Remaining Experiments COMPLETE"
echo "  Results in: experiments/results-v5/"
echo "=============================================="
