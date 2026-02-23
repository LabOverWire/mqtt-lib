#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="06_resource_overhead"
STRATEGIES=("control-only" "per-publish" "per-topic" "per-subscription")
CONCURRENCIES=(10 50 100)
RUNS_PER_DATAPOINT=3

start_broker "--quic-host 0.0.0.0:14567 --quic-cert /opt/mqtt-certs/server.pem --quic-key /opt/mqtt-certs/server.key"

for conc in "${CONCURRENCIES[@]}"; do
    label="tcp_${conc}conn"
    echo "[${EXPERIMENT}] ${label}"
    monitor_file="${RESULTS_DIR}/${EXPERIMENT}/${label}_resources.csv"
    start_monitor "$monitor_file"
    run_repeated "$EXPERIMENT" "$label" \
        "--url mqtt://${BROKER_IP}:1883 --mode throughput --duration 60 --publishers ${conc} --subscribers ${conc}"
    stop_monitor "$monitor_file"

    for strategy in "${STRATEGIES[@]}"; do
        label="quic-${strategy}_${conc}conn"
        echo "[${EXPERIMENT}] ${label}"
        monitor_file="${RESULTS_DIR}/${EXPERIMENT}/${label}_resources.csv"
        start_monitor "$monitor_file"
        run_repeated "$EXPERIMENT" "$label" \
            "--url quic://${BROKER_IP}:14567 --insecure --quic-stream-strategy ${strategy} --mode throughput --duration 60 --publishers ${conc} --subscribers ${conc}"
        stop_monitor "$monitor_file"
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete"
