#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="03_throughput_under_loss"
LOSSES=(0 1 2 5 10)
DELAY=10
QOS_LEVELS=(0 1)
STRATEGIES=("control-only" "per-publish" "per-topic" "per-subscription")

start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"

for qos in "${QOS_LEVELS[@]}"; do
    for loss in "${LOSSES[@]}"; do
        apply_netem "$DELAY" "$loss"

        label="tcp_qos${qos}_loss${loss}pct"
        echo "[${EXPERIMENT}] ${label}"
        run_monitored "$EXPERIMENT" "$label" \
            "--url mqtt://${BROKER_IP}:1883 --mode throughput --duration 30 --warmup 5 --payload-size 256 --qos ${qos} --publishers 4 --subscribers 4"

        for strategy in "${STRATEGIES[@]}"; do
            label="quic-${strategy}_qos${qos}_loss${loss}pct"
            echo "[${EXPERIMENT}] ${label}"
            run_monitored "$EXPERIMENT" "$label" \
                "--url quic://${BROKER_IP}:14567 --ca-cert /opt/mqtt-certs/ca.pem --quic-stream-strategy ${strategy} --mode throughput --duration 30 --warmup 5 --payload-size 256 --qos ${qos} --publishers 4 --subscribers 4"
        done

        clear_netem
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete"
