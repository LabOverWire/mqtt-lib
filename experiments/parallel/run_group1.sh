#!/usr/bin/env bash
set -euo pipefail
export GROUP=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOG="${SCRIPT_DIR}/group1.log"

echo "=== Group 1: Exp 02 (HOL blocking, 15 runs) ===" | tee "$LOG"
bash "${SCRIPT_DIR}/02_hol_blocking.sh" 2>&1 | tee -a "$LOG"

echo "=== Group 1 complete ===" | tee -a "$LOG"
