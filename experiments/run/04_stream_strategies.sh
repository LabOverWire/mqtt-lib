#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="04_stream_strategies"
STRATEGIES=("control-only" "per-publish" "per-topic" "per-subscription")
TOPIC_COUNTS=(1 4 8 16)
DELAY=25
LOSS=2

start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"
apply_netem "$DELAY" "$LOSS"

for strategy in "${STRATEGIES[@]}"; do
    for ntopics in "${TOPIC_COUNTS[@]}"; do
        label="${strategy}_${ntopics}topics_throughput"
        echo "[${EXPERIMENT}] ${label}"
        run_monitored "$EXPERIMENT" "$label" \
            "--url quic://${BROKER_IP}:14567 --ca-cert /opt/mqtt-certs/ca.pem --quic-stream-strategy ${strategy} --mode throughput --duration 30 --warmup 5 --publishers ${ntopics} --subscribers ${ntopics}"

        label="${strategy}_${ntopics}topics_latency"
        echo "[${EXPERIMENT}] ${label}"
        run_monitored "$EXPERIMENT" "$label" \
            "--url quic://${BROKER_IP}:14567 --ca-cert /opt/mqtt-certs/ca.pem --quic-stream-strategy ${strategy} --mode latency --duration 30 --warmup 5 --load-factor 0.7"
    done
done

clear_netem
stop_broker
echo "experiment ${EXPERIMENT} complete"
