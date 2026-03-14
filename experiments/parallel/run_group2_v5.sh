#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=== Group 2: HOL v4 perpub-isolated → Exp 3 (throughput under loss) ==="

echo "[Group 2] Starting HOL v4 (perpub-isolated)..."
V4_TRANSPORTS="quic-perpub-isolated" GROUP=2 bash "${SCRIPT_DIR}/02_hol_blocking_v5_fp.sh"
echo "[Group 2] HOL v4 complete"

echo "[Group 2] Starting Exp 3 (throughput under loss)..."
GROUP=2 bash "${SCRIPT_DIR}/03_throughput_under_loss_v5.sh"
echo "[Group 2] Exp 3 complete"

echo "=== Group 2 all experiments complete ==="
