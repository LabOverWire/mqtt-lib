#!/usr/bin/env bash
set -euo pipefail

LOSS_PCT="${1:?usage: $0 <loss_pct> [delay_us]}"
DELAY_US="${2:-0}"

DEV=$(ip route show default | awk '{print $5}' | head -1)
PARENT=$(cat /tmp/router_netem_parent)
tc qdisc change dev "$DEV" parent "$PARENT" handle 10: netem delay "${DELAY_US}us" loss "${LOSS_PCT}%" limit 100000
echo "router netem: dev=${DEV} parent=${PARENT} delay=${DELAY_US}us loss=${LOSS_PCT}%"
