#!/usr/bin/env bash
set -euo pipefail

INTERVAL="${1:-1}"

exec 9>/tmp/router_monitor.lock
if ! flock -w 5 9; then
    echo "router_monitor already running" >&2
    exit 1
fi

snapshot() {
    awk '/^cpu[0-9]+ / {
        busy = $2 + $3 + $4 + $7 + $8 + $9
        print $1, busy, busy + $5 + $6, $4, $8
    }' /proc/stat
}

echo "timestamp,cpu,busy_pct,sys_pct,softirq_pct"

prev=$(snapshot)
sleep "$INTERVAL" 9>&-

while true; do
    ts=$(date +%s)
    cur=$(snapshot)
    paste -d' ' <(echo "$prev") <(echo "$cur") | awk -v ts="$ts" '{
        total = $8 - $3
        if (total <= 0) total = 1
        printf "%s,%s,%.1f,%.1f,%.1f\n", ts, $1, 100*($7 - $2)/total, 100*($9 - $4)/total, 100*($10 - $5)/total
    }'
    prev=$cur
    sleep "$INTERVAL" 9>&-
done
