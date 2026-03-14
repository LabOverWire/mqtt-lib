#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export GROUP=3

echo "=== Starting Exp 6 (resource overhead) ==="
bash "${SCRIPT_DIR}/06_resource_overhead_v5.sh"

echo "=== Starting Exp 1 (connection latency) ==="
bash "${SCRIPT_DIR}/01_connection_latency_v5.sh"

echo "=== Group 3 restart ALL DONE ==="
