#!/usr/bin/env bash
set -euo pipefail

DELAY_US="${1:?usage: $0 <delay_us> <loss_pct> [limit]}"
LOSS_PCT="${2:-0}"
LIMIT="${3:-}"

DEV=$(ip route show default | awk '{print $5}' | head -1)
tc qdisc del dev "$DEV" root 2>/dev/null || true
tc qdisc add dev "$DEV" root handle 10: netem delay "${DELAY_US}us" loss "${LOSS_PCT}%" ${LIMIT:+limit "$LIMIT"}
echo "broker netem: dev=${DEV} delay=${DELAY_US}us loss=${LOSS_PCT}% limit=${LIMIT:-default}"
