#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=== MQoQ Benchmark Suite ==="
echo "started at $(date -u)"
echo ""

for script in "$SCRIPT_DIR"/0[1-6]_*.sh; do
    name=$(basename "$script")
    echo "=========================================="
    echo "running ${name}..."
    echo "=========================================="
    bash "$script"
    echo ""
done

echo "=== all experiments complete ==="
echo "finished at $(date -u)"
