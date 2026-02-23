#!/usr/bin/env bash
set -euo pipefail

DEV=$(ip route show default | awk '{print $5}' | head -1)
tc qdisc del dev "$DEV" root 2>/dev/null || true
echo "netem: cleared dev=${DEV}"
