#!/usr/bin/env bash
set -euo pipefail

DELAY_MS="${1:?usage: $0 <delay_ms> <loss_pct>}"
LOSS_PCT="${2:-0}"

DEV=$(ip route show default | awk '{print $5}' | head -1)
tc qdisc replace dev "$DEV" root netem delay "${DELAY_MS}ms" loss "${LOSS_PCT}%"
echo "netem: dev=${DEV} delay=${DELAY_MS}ms loss=${LOSS_PCT}%"
