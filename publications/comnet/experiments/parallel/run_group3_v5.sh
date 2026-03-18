#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=== Group 3: Exp 4 → Exp 6 → Exp 1 ==="

echo "[Group 3] Starting Exp 4 (stream strategies)..."
GROUP=3 bash "${SCRIPT_DIR}/04_stream_strategies_v5.sh"
echo "[Group 3] Exp 4 complete"

echo "[Group 3] Starting Exp 6 (resource overhead)..."
GROUP=3 bash "${SCRIPT_DIR}/06_resource_overhead_v5.sh"
echo "[Group 3] Exp 6 complete"

echo "[Group 3] Starting Exp 1 (connection latency)..."
GROUP=3 bash "${SCRIPT_DIR}/01_connection_latency_v5.sh"
echo "[Group 3] Exp 1 complete"

echo "=== Group 3 all experiments complete ==="
