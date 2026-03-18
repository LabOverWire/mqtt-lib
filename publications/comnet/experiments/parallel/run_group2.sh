#!/usr/bin/env bash
set -euo pipefail
export GROUP=2

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOG="${SCRIPT_DIR}/group2.log"

echo "=== Group 2: Exp 01, 03, 05 ===" | tee "$LOG"

echo "--- Exp 01: connection latency ---" | tee -a "$LOG"
bash "${SCRIPT_DIR}/01_connection_latency.sh" 2>&1 | tee -a "$LOG"

echo "--- Exp 03: throughput under loss ---" | tee -a "$LOG"
bash "${SCRIPT_DIR}/03_throughput_under_loss.sh" 2>&1 | tee -a "$LOG"

echo "--- Exp 05: datagram vs stream ---" | tee -a "$LOG"
bash "${SCRIPT_DIR}/05_datagram_vs_stream.sh" 2>&1 | tee -a "$LOG"

echo "=== Group 2 complete ===" | tee -a "$LOG"
