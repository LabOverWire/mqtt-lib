#!/usr/bin/env bash
set -euo pipefail

DEV=$(ip route show default | awk '{print $5}' | head -1)
tc -s filter show dev "$DEV" ingress | awk '/Sent [0-9]+ bytes [0-9]+ pkt/ { for (i = 1; i <= NF; i++) if ($i == "pkt") total += $(i - 1) } END { print total + 0 }'
