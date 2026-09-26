#!/usr/bin/env bash
set -euo pipefail

PID="${1:?usage: $0 <pid>}"
INTERVAL="${2:-1}"

exec 9>/tmp/resource_monitor.lock
if ! flock -w 5 9; then
    echo "resource_monitor already running" >&2
    exit 1
fi

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

read_host_cpu() {
    awk '/^cpu / {print $2, $3, $4, $5, $6, $7, $8, $9}' /proc/stat
}

echo "timestamp,rss_kb,cpu_percent,threads,net_rx_bytes,net_tx_bytes,net_rx_packets,net_tx_packets,host_user,host_nice,host_sys,host_idle,host_iowait,host_irq,host_softirq,host_steal,host_busy"

prev_jiffies=$(read_cpu_jiffies)
prev_time=$(date +%s.%N)
read -r p_user p_nice p_sys p_idle p_iowait p_irq p_softirq p_steal <<< "$(read_host_cpu)"

while kill -0 "$PID" 2>/dev/null; do
    sleep "$INTERVAL" 9>&-
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
    read -r rx_bytes rx_packets tx_bytes tx_packets <<< "$(read_net_counters)"
    : "${rx_bytes:=0}" "${rx_packets:=0}" "${tx_bytes:=0}" "${tx_packets:=0}"
    read -r c_user c_nice c_sys c_idle c_iowait c_irq c_softirq c_steal <<< "$(read_host_cpu)"
    host=$(awk -v du=$((c_user - p_user)) -v dn=$((c_nice - p_nice)) -v ds=$((c_sys - p_sys)) \
        -v di=$((c_idle - p_idle)) -v dw=$((c_iowait - p_iowait)) -v dq=$((c_irq - p_irq)) \
        -v dsq=$((c_softirq - p_softirq)) -v dst=$((c_steal - p_steal)) \
        'BEGIN {
            total = du + dn + ds + di + dw + dq + dsq + dst
            if (total <= 0) { total = 1; di = 1 }
            printf "%.1f,%.1f,%.1f,%.1f,%.1f,%.1f,%.1f,%.1f,%.1f", 100*du/total, 100*dn/total, 100*ds/total,
                100*di/total, 100*dw/total, 100*dq/total, 100*dsq/total, 100*dst/total, 100*(total - di - dw)/total
        }')
    p_user=$c_user; p_nice=$c_nice; p_sys=$c_sys; p_idle=$c_idle
    p_iowait=$c_iowait; p_irq=$c_irq; p_softirq=$c_softirq; p_steal=$c_steal
    echo "${ts},${rss},${cpu},${threads},${rx_bytes},${tx_bytes},${rx_packets},${tx_packets},${host}"
done
