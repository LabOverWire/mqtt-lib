#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

EXPERIMENT="03_throughput_under_loss"
LOSSES=(0 5)
DELAY=10
QOS_LEVELS=(0 1)
STRATEGIES=("per-topic")
RUNS_PER_DATAPOINT=2

RESULTS_DIR="${ROOT_DIR}/results-watched"
mkdir -p "$RESULTS_DIR"

BENCH_COMMON="--mode throughput --duration 30 --warmup 5 --payload-size 256 --publishers 16 --subscribers 8 --inflight 64"

start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"

for qos in "${QOS_LEVELS[@]}"; do
    for loss in "${LOSSES[@]}"; do
        apply_netem "$DELAY" "$loss"

        label="tcp_qos${qos}_loss${loss}pct"
        echo "[${EXPERIMENT}] ${label}"
        run_monitored_split "$EXPERIMENT" "$label" \
            "--url mqtt://${BROKER_IP}:1883 ${BENCH_COMMON} --qos ${qos}"

        for strategy in "${STRATEGIES[@]}"; do
            label="quic-${strategy}_qos${qos}_loss${loss}pct"
            echo "[${EXPERIMENT}] ${label}"
            run_monitored_split "$EXPERIMENT" "$label" \
                "--url quic://${BROKER_IP}:14567 --ca-cert /opt/mqtt-certs/ca.pem --quic-stream-strategy ${strategy} ${BENCH_COMMON} --qos ${qos}"
        done

        clear_netem
    done
done

stop_broker
echo "watched slice complete (group ${GROUP})"
