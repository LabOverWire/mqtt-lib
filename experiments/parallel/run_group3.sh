#!/usr/bin/env bash
set -euo pipefail
export GROUP=3

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOG="${SCRIPT_DIR}/group3.log"

echo "=== Group 3: Exp 04, 06 ===" | tee "$LOG"

echo "--- Exp 04: stream strategies ---" | tee -a "$LOG"
bash "${SCRIPT_DIR}/04_stream_strategies.sh" 2>&1 | tee -a "$LOG"

echo "--- Exp 06: resource overhead ---" | tee -a "$LOG"
bash "${SCRIPT_DIR}/06_resource_overhead.sh" 2>&1 | tee -a "$LOG"

echo "=== Group 3 complete ===" | tee -a "$LOG"
