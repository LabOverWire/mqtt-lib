#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="01_connection_latency"
TRANSPORTS=("mqtt://${BROKER_IP}:1883" "mqtts://${BROKER_IP}:8883" "quic://${BROKER_IP}:14567")
TRANSPORT_NAMES=("tcp" "tls" "quic")
DELAYS=(0 25 50 100 200)

for tidx in "${!TRANSPORTS[@]}"; do
    url="${TRANSPORTS[$tidx]}"
    tname="${TRANSPORT_NAMES[$tidx]}"

    if [ "$tname" = "tls" ]; then
        start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key"
    elif [ "$tname" = "quic" ]; then
        start_broker "--quic-host 0.0.0.0:14567 --quic-cert /opt/mqtt-certs/server.pem --quic-key /opt/mqtt-certs/server.key"
    else
        start_broker ""
    fi

    for delay in "${DELAYS[@]}"; do
        apply_netem "$delay" 0
        label="${tname}_delay${delay}ms"
        echo "[${EXPERIMENT}] ${label}"
        run_repeated "$EXPERIMENT" "$label" \
            "--url ${url} --insecure --mode connections --concurrency 1 --duration 30"
        clear_netem
    done

    stop_broker
done

echo "experiment ${EXPERIMENT} complete"
