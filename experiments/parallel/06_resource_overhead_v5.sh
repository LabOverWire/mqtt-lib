#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

EXPERIMENT="06_resource_overhead"
STRATEGIES=("control-only" "per-publish" "per-topic")
CONCURRENCIES=(10 50 100)
RUNS_PER_DATAPOINT=15
BROKER_FLAGS="--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"

RESULTS_DIR="${ROOT_DIR}/results-v5"
mkdir -p "$RESULTS_DIR"

for conc in "${CONCURRENCIES[@]}"; do
    label="tcp_${conc}conn"
    echo "[${EXPERIMENT}] ${label}"
    for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
        run_label="${label}_run${run}"
        stop_broker 2>/dev/null || true
        start_broker "${BROKER_FLAGS}"
        start_monitors
        run_bench_split "$EXPERIMENT" "$run_label" \
            "--url mqtt://${BROKER_IP}:1883 --mode throughput --duration 60 --warmup 5 --payload-size 256 --publishers ${conc} --subscribers ${conc}"
        stop_monitors "${RESULTS_DIR}/${EXPERIMENT}" "$run_label"
        sleep 2
    done

    for strategy in "${STRATEGIES[@]}"; do
        label="quic-${strategy}_${conc}conn"
        echo "[${EXPERIMENT}] ${label}"
        for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
            run_label="${label}_run${run}"
            stop_broker 2>/dev/null || true
            start_broker "${BROKER_FLAGS}"
            start_monitors
            run_bench_split "$EXPERIMENT" "$run_label" \
                "--url quic://${BROKER_IP}:14567 --ca-cert /opt/mqtt-certs/ca.pem --quic-stream-strategy ${strategy} --mode throughput --duration 60 --warmup 5 --payload-size 256 --publishers ${conc} --subscribers ${conc}"
            stop_monitors "${RESULTS_DIR}/${EXPERIMENT}" "$run_label"
            sleep 2
        done
    done
done

stop_broker
echo "experiment ${EXPERIMENT} v5 complete (group ${GROUP})"
