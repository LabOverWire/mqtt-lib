#!/usr/bin/env bash
set -euo pipefail

INTERVAL="${1:-1}"

DEV=$(ip route show default | awk '{print $5}' | head -1)
echo "timestamp,backlog_bytes,backlog_pkts,dropped"
while true; do
    tc -s -j qdisc show dev "$DEV" | python3 -c '
import json, sys, time
for qdisc in json.load(sys.stdin):
    if qdisc.get("kind") == "netem" and qdisc.get("handle") == "10:":
        print(f"{int(time.time())},{qdisc.get("backlog", 0)},{qdisc.get("qlen", 0)},{qdisc.get("drops", 0)}")
'
    sleep "$INTERVAL"
done
