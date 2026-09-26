#!/usr/bin/env bash
set -euo pipefail

DELAY_MS="${1:?usage: $0 <delay_ms> <rate_mbit> [limit_pkts] [loss_pct]}"
RATE_MBIT="${2:?usage: $0 <delay_ms> <rate_mbit> [limit_pkts] [loss_pct]}"
LIMIT_PKTS="${3:-1000}"
LOSS_PCT="${4:-0}"

DEV=$(ip route show default | awk '{print $5}' | head -1)
tc qdisc replace dev "$DEV" root netem delay "${DELAY_MS}ms" loss "${LOSS_PCT}%" rate "${RATE_MBIT}mbit" limit "${LIMIT_PKTS}"
echo "netem-bottleneck: dev=${DEV} delay=${DELAY_MS}ms rate=${RATE_MBIT}mbit limit=${LIMIT_PKTS}pkts loss=${LOSS_PCT}%"
