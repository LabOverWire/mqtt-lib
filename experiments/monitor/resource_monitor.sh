#!/usr/bin/env bash
set -euo pipefail

PID="${1:?usage: $0 <pid>}"
INTERVAL="${2:-1}"

IFACE=$(ip route show default 2>/dev/null | awk '{print $5; exit}')
: "${IFACE:=eth0}"

read_net_counters() {
    awk -v iface="${IFACE}:" '$1 == iface {print $2, $3, $10, $11}' /proc/net/dev
}

echo "timestamp,rss_kb,cpu_percent,threads,net_rx_bytes,net_tx_bytes,net_rx_packets,net_tx_packets"

while kill -0 "$PID" 2>/dev/null; do
    ts=$(date +%s)
    rss=$(awk '/^VmRSS:/ {print $2}' "/proc/${PID}/status" 2>/dev/null || echo 0)
    cpu=$(ps -p "$PID" -o %cpu= 2>/dev/null | tr -d ' ' || echo 0)
    threads=$(awk '/^Threads:/ {print $2}' "/proc/${PID}/status" 2>/dev/null || echo 0)
    net=$(read_net_counters)
    rx_bytes=$(echo "$net" | awk '{print $1}')
    rx_packets=$(echo "$net" | awk '{print $2}')
    tx_bytes=$(echo "$net" | awk '{print $3}')
    tx_packets=$(echo "$net" | awk '{print $4}')
    : "${rx_bytes:=0}" "${rx_packets:=0}" "${tx_bytes:=0}" "${tx_packets:=0}"
    echo "${ts},${rss},${cpu},${threads},${rx_bytes},${tx_bytes},${rx_packets},${tx_packets}"
    sleep "$INTERVAL"
done
