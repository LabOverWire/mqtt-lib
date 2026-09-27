#!/usr/bin/env bash
set -euo pipefail

INTERVAL="${1:-1}"

exec 9>/tmp/client_monitor.lock
if ! flock -w 5 9; then
    echo "client_monitor already running" >&2
    exit 1
fi

IFACE=$(ip route show default 2>/dev/null | awk '{print $5; exit}')
: "${IFACE:=eth0}"

read_cpu() {
    awk '/^cpu / {print $2, $3, $4, $5, $6, $7, $8, $9}' /proc/stat
}

read_net_counters() {
    awk -v iface="${IFACE}:" '$1 == iface {print $2, $3, $10, $11}' /proc/net/dev
}

echo "timestamp,cpu_user,cpu_sys,cpu_idle,net_rx_bytes,net_tx_bytes,net_rx_packets,net_tx_packets,cpu_nice,cpu_iowait,cpu_irq,cpu_softirq,cpu_steal,cpu_busy"

read -r p_user p_nice p_sys p_idle p_iowait p_irq p_softirq p_steal <<< "$(read_cpu)"

sleep "$INTERVAL" 9>&-

while true; do
    ts=$(date +%s)
    read -r c_user c_nice c_sys c_idle c_iowait c_irq c_softirq c_steal <<< "$(read_cpu)"
    read -r rx_bytes rx_packets tx_bytes tx_packets <<< "$(read_net_counters)"
    : "${rx_bytes:=0}" "${rx_packets:=0}" "${tx_bytes:=0}" "${tx_packets:=0}"

    awk -v ts="$ts" \
        -v du=$((c_user - p_user)) -v dn=$((c_nice - p_nice)) -v ds=$((c_sys - p_sys)) \
        -v di=$((c_idle - p_idle)) -v dw=$((c_iowait - p_iowait)) -v dq=$((c_irq - p_irq)) \
        -v dsq=$((c_softirq - p_softirq)) -v dst=$((c_steal - p_steal)) \
        -v rxb="$rx_bytes" -v txb="$tx_bytes" -v rxp="$rx_packets" -v txp="$tx_packets" \
        'BEGIN {
            total = du + dn + ds + di + dw + dq + dsq + dst
            if (total <= 0) { total = 1; di = 1 }
            printf "%s,%.1f,%.1f,%.1f,%s,%s,%s,%s,%.1f,%.1f,%.1f,%.1f,%.1f,%.1f\n", ts,
                100*du/total, 100*ds/total, 100*di/total, rxb, txb, rxp, txp,
                100*dn/total, 100*dw/total, 100*dq/total, 100*dsq/total, 100*dst/total,
                100*(total - di - dw)/total
        }'

    p_user=$c_user; p_nice=$c_nice; p_sys=$c_sys; p_idle=$c_idle
    p_iowait=$c_iowait; p_irq=$c_irq; p_softirq=$c_softirq; p_steal=$c_steal
    sleep "$INTERVAL" 9>&-
done
