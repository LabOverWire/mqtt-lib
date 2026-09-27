#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

EXPERIMENT="03_throughput_under_loss"
LOSSES=(${LOSSES_OVERRIDE:-0 1 2 5 10})
DELAY=10
QOS_LEVELS=(${QOS_OVERRIDE:-0 1})
RUNS_PER_DATAPOINT=15

RESULTS_DIR="${ROOT_DIR}/results-v5"
OUTPUT_DIR="${RESULTS_DIR}/${EXPERIMENT}"
mkdir -p "$OUTPUT_DIR"

run_tls_cell() {
    local label="$1"
    shift
    local bench_args="$*"
    for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
        local run_label="${label}_run${run}"
        local f="${OUTPUT_DIR}/${run_label}.json"
        if [ -f "$f" ] && [ "$(wc -c < "$f")" -gt 300 ]; then
            continue
        fi
        if [ "$BROKER_FRESH" = "1" ]; then
            BROKER_FRESH=0
        elif ! restart_broker; then
            echo "WARN: broker restart failed, skipping ${run_label}" >&2
            continue
        fi
        start_monitors
        run_bench_split "$EXPERIMENT" "$run_label" "$bench_args"
        stop_monitors "$OUTPUT_DIR" "$run_label"
        sleep 5
    done
}

start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"

for qos in "${QOS_LEVELS[@]}"; do
    for loss in "${LOSSES[@]}"; do
        apply_netem "$DELAY" "$loss"
        label="tls_qos${qos}_loss${loss}pct"
        echo "[${EXPERIMENT}] ${label}"
        run_tls_cell "$label" \
            "--url mqtts://${BROKER_IP}:8883 --ca-cert /opt/mqtt-certs/ca.pem --mode throughput --duration 60 --warmup 5 --payload-size 256 --qos ${qos} --publishers 16 --subscribers 8 --inflight 64"
        clear_netem
    done
done

stop_broker
echo "experiment ${EXPERIMENT} TLS arm complete (group ${GROUP})"
