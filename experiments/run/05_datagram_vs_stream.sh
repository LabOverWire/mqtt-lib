#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="05_datagram_vs_stream"
LOSSES=(0 1 5 10)
DELAY=10

start_broker "--quic-host 0.0.0.0:14567 --quic-cert /opt/mqtt-certs/server.pem --quic-key /opt/mqtt-certs/server.key"

for loss in "${LOSSES[@]}"; do
    apply_netem "$DELAY" "$loss"

    label="quic-stream_loss${loss}pct"
    echo "[${EXPERIMENT}] ${label}"
    run_repeated "$EXPERIMENT" "$label" \
        "--url quic://${BROKER_IP}:14567 --insecure --mode latency --qos 0 --duration 30"

    label="quic-datagram_loss${loss}pct"
    echo "[${EXPERIMENT}] ${label}"
    run_repeated "$EXPERIMENT" "$label" \
        "--url quic://${BROKER_IP}:14567 --insecure --quic-datagrams --mode latency --qos 0 --duration 30"

    clear_netem
done

stop_broker
echo "experiment ${EXPERIMENT} complete"
