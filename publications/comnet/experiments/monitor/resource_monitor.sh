#!/usr/bin/env bash
set -euo pipefail

PID="${1:?usage: $0 <pid>}"
INTERVAL="${2:-1}"

IFACE=$(ip route show default 2>/dev/null | awk '{print $5; exit}')
: "${IFACE:=eth0}"

CLK_TCK=$(getconf CLK_TCK 2>/dev/null || echo 100)

read_net_counters() {
    awk -v iface="${IFACE}:" '$1 == iface {print $2, $3, $10, $11}' /proc/net/dev
}

read_cpu_jiffies() {
    awk '{ s=$0; sub(/^.*\) /, "", s); split(s, f, " "); print f[12] + f[13] }' \
        "/proc/${PID}/stat" 2>/dev/null || echo 0
}

echo "timestamp,rss_kb,cpu_percent,threads,net_rx_bytes,net_tx_bytes,net_rx_packets,net_tx_packets"

prev_jiffies=$(read_cpu_jiffies)
prev_time=$(date +%s.%N)

while kill -0 "$PID" 2>/dev/null; do
    sleep "$INTERVAL"
    kill -0 "$PID" 2>/dev/null || break
    now=$(date +%s.%N)
    ts="${now%.*}"
    cur_jiffies=$(read_cpu_jiffies)
    cpu=$(awk -v cj="$cur_jiffies" -v pj="$prev_jiffies" -v t1="$prev_time" -v t2="$now" -v hz="$CLK_TCK" \
        'BEGIN { dt = t2 - t1; if (dt <= 0 || cj < pj) { print "0.0" } else { printf "%.1f", 100 * ((cj - pj) / hz) / dt } }')
    prev_jiffies=$cur_jiffies
    prev_time=$now
    rss=$(awk '/^VmRSS:/ {print $2}' "/proc/${PID}/status" 2>/dev/null || echo 0)
    threads=$(awk '/^Threads:/ {print $2}' "/proc/${PID}/status" 2>/dev/null || echo 0)
    net=$(read_net_counters)
    rx_bytes=$(echo "$net" | awk '{print $1}')
    rx_packets=$(echo "$net" | awk '{print $2}')
    tx_bytes=$(echo "$net" | awk '{print $3}')
    tx_packets=$(echo "$net" | awk '{print $4}')
    : "${rx_bytes:=0}" "${rx_packets:=0}" "${tx_bytes:=0}" "${tx_packets:=0}"
    echo "${ts},${rss},${cpu},${threads},${rx_bytes},${tx_bytes},${rx_packets},${tx_packets}"
done
