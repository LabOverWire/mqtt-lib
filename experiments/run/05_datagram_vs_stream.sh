#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="05_datagram_vs_stream"
LOSSES=(0 1 5 10)
DELAY=10

start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"

for loss in "${LOSSES[@]}"; do
    apply_netem "$DELAY" "$loss"

    for mode in latency throughput; do
        label="quic-stream_loss${loss}pct_${mode}"
        echo "[${EXPERIMENT}] ${label}"
        run_monitored "$EXPERIMENT" "$label" \
            "--url quic://${BROKER_IP}:14567 --ca-cert /opt/mqtt-certs/ca.pem --mode ${mode} --qos 0 --duration 30 --warmup 5 --payload-size 256"

        label="quic-datagram_loss${loss}pct_${mode}"
        echo "[${EXPERIMENT}] ${label}"
        run_monitored "$EXPERIMENT" "$label" \
            "--url quic://${BROKER_IP}:14567 --ca-cert /opt/mqtt-certs/ca.pem --quic-datagrams --mode ${mode} --qos 0 --duration 30 --warmup 5 --payload-size 256"
    done

    clear_netem
done

stop_broker
echo "experiment ${EXPERIMENT} complete"
