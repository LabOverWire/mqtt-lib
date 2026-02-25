#!/usr/bin/env bash
set -euo pipefail

INTERVAL="${1:-1}"

IFACE=$(ip route show default 2>/dev/null | awk '{print $5; exit}')
: "${IFACE:=eth0}"

read_cpu() {
    awk '/^cpu / {print $2, $4, $5}' /proc/stat
}

read_net_counters() {
    awk -v iface="${IFACE}:" '$1 == iface {print $2, $3, $10, $11}' /proc/net/dev
}

echo "timestamp,cpu_user,cpu_sys,cpu_idle,net_rx_bytes,net_tx_bytes,net_rx_packets,net_tx_packets"

prev_cpu=$(read_cpu)
prev_user=$(echo "$prev_cpu" | awk '{print $1}')
prev_sys=$(echo "$prev_cpu" | awk '{print $2}')
prev_idle=$(echo "$prev_cpu" | awk '{print $3}')

sleep "$INTERVAL"

while true; do
    ts=$(date +%s)

    cur_cpu=$(read_cpu)
    cur_user=$(echo "$cur_cpu" | awk '{print $1}')
    cur_sys=$(echo "$cur_cpu" | awk '{print $2}')
    cur_idle=$(echo "$cur_cpu" | awk '{print $3}')

    d_user=$((cur_user - prev_user))
    d_sys=$((cur_sys - prev_sys))
    d_idle=$((cur_idle - prev_idle))
    d_total=$((d_user + d_sys + d_idle))

    if [ "$d_total" -gt 0 ]; then
        cpu_user=$(awk "BEGIN {printf \"%.1f\", 100*${d_user}/${d_total}}")
        cpu_sys=$(awk "BEGIN {printf \"%.1f\", 100*${d_sys}/${d_total}}")
        cpu_idle=$(awk "BEGIN {printf \"%.1f\", 100*${d_idle}/${d_total}}")
    else
        cpu_user="0.0"
        cpu_sys="0.0"
        cpu_idle="100.0"
    fi

    prev_user=$cur_user
    prev_sys=$cur_sys
    prev_idle=$cur_idle

    net=$(read_net_counters)
    rx_bytes=$(echo "$net" | awk '{print $1}')
    rx_packets=$(echo "$net" | awk '{print $2}')
    tx_bytes=$(echo "$net" | awk '{print $3}')
    tx_packets=$(echo "$net" | awk '{print $4}')
    : "${rx_bytes:=0}" "${rx_packets:=0}" "${tx_bytes:=0}" "${tx_packets:=0}"

    echo "${ts},${cpu_user},${cpu_sys},${cpu_idle},${rx_bytes},${tx_bytes},${rx_packets},${tx_packets}"
    sleep "$INTERVAL"
done
