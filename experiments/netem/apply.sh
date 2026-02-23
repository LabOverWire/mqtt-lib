#!/usr/bin/env bash
set -euo pipefail

DELAY_MS="${1:?usage: $0 <delay_ms> <loss_pct>}"
LOSS_PCT="${2:-0}"

tc qdisc replace dev eth0 root netem delay "${DELAY_MS}ms" loss "${LOSS_PCT}%"
echo "netem: delay=${DELAY_MS}ms loss=${LOSS_PCT}%"
