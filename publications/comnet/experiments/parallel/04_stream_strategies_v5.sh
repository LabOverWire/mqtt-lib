#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

EXPERIMENT="04_stream_strategies"
STRATEGIES=("control-only" "per-publish" "per-topic")
TOPIC_COUNTS=(1 4 8 16)
DELAY=25
LOSS=2
RUNS_PER_DATAPOINT=15

RESULTS_DIR="${ROOT_DIR}/results-v5"
mkdir -p "$RESULTS_DIR"

for strategy in "${STRATEGIES[@]}"; do
    clear_netem 2>/dev/null || true
    stop_broker 2>/dev/null || true
    if ! start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567 --quic-delivery-strategy ${strategy}"; then
        echo "WARN: broker start failed for strategy ${strategy}, skipping" >&2
        continue
    fi
    apply_netem "$DELAY" "$LOSS"
    for ntopics in "${TOPIC_COUNTS[@]}"; do
        label="${strategy}_${ntopics}topics_throughput"
        echo "[${EXPERIMENT}] ${label}"
        run_monitored_split "$EXPERIMENT" "$label" \
            "--url quic://${BROKER_IP}:14567 --ca-cert /opt/mqtt-certs/ca.pem --quic-stream-strategy ${strategy} --mode throughput --duration 60 --warmup 5 --payload-size 256 --publishers 1 --topics ${ntopics} --subscribers 1"
    done
done

clear_netem
stop_broker
echo "experiment ${EXPERIMENT} v5 complete (group ${GROUP})"
