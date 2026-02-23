#!/usr/bin/env bash
set -euo pipefail

PID="${1:?usage: $0 <pid>}"
INTERVAL="${2:-1}"

echo "timestamp,rss_kb,cpu_percent,threads"

while kill -0 "$PID" 2>/dev/null; do
    ts=$(date +%s)
    rss=$(awk '/^VmRSS:/ {print $2}' "/proc/${PID}/status" 2>/dev/null || echo 0)
    cpu=$(ps -p "$PID" -o %cpu= 2>/dev/null | tr -d ' ' || echo 0)
    threads=$(awk '/^Threads:/ {print $2}' "/proc/${PID}/status" 2>/dev/null || echo 0)
    echo "${ts},${rss},${cpu},${threads}"
    sleep "$INTERVAL"
done
