#!/usr/bin/env bash
set -euo pipefail

DELAY_US="${1:?usage: $0 <delay_us> <loss_pct>}"
LOSS_PCT="${2:-0}"

DEV=$(ip route show default | awk '{print $5}' | head -1)
MTU=$(cat "/sys/class/net/${DEV}/mtu")
BURST=$((MTU + 14))
tc qdisc del dev "$DEV" root 2>/dev/null || true
tc qdisc add dev "$DEV" root handle 1: tbf rate 100gbit burst "$BURST" latency 100ms
tc qdisc add dev "$DEV" parent 1:1 handle 10: netem delay "${DELAY_US}us" loss "${LOSS_PCT}%" limit 100000
echo "broker tbf+netem: dev=${DEV} burst=${BURST} delay=${DELAY_US}us loss=${LOSS_PCT}%"
