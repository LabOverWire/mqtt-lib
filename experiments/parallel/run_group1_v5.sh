#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=== Group 1: HOL v4 pertopic-isolated → Exp 5 (datagram vs stream) ==="

echo "[Group 1] Starting HOL v4 (pertopic-isolated)..."
V4_TRANSPORTS="quic-pertopic-isolated" GROUP=1 bash "${SCRIPT_DIR}/02_hol_blocking_v5_fp.sh"
echo "[Group 1] HOL v4 complete"

echo "[Group 1] Starting Exp 5 (datagram vs stream)..."
GROUP=1 bash "${SCRIPT_DIR}/05_datagram_vs_stream_v5.sh"
echo "[Group 1] Exp 5 complete"

echo "=== Group 1 all experiments complete ==="
