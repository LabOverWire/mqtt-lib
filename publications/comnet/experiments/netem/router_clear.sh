#!/usr/bin/env bash
set -euo pipefail

DEV=$(ip route show default | awk '{print $5}' | head -1)
tc qdisc del dev "$DEV" root 2>/dev/null || true
tc qdisc del dev "$DEV" clsact 2>/dev/null || true
rm -f /tmp/router_netem_parent
echo "router: cleared dev=${DEV}"
