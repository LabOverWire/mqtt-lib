#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="13_payload_format_remote"
FORMATS=(raw json bebytes compressed-json)
SIZES=(64 256 1024 4096)
DURATION=30
WARMUP=5
QOS=0

start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key"

for fmt in "${FORMATS[@]}"; do
    for size in "${SIZES[@]}"; do
        for mode in latency throughput; do
            label="${fmt}_${size}b_${mode}"
            echo "[${EXPERIMENT}] ${label}"
            run_monitored "$EXPERIMENT" "$label" \
                "--url mqtts://${BROKER_IP}:8883 --ca-cert /opt/mqtt-certs/ca.pem --mode ${mode} --duration ${DURATION} --warmup ${WARMUP} --payload-size ${size} --payload-format ${fmt} --qos ${QOS}"
        done
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete"
