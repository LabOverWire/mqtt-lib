#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="01_connection_latency"
TRANSPORTS=("mqtt://${BROKER_IP}:1883" "mqtts://${BROKER_IP}:8883" "quic://${BROKER_IP}:14567")
TRANSPORT_NAMES=("tcp" "tls" "quic")
DELAYS=(0 25 50 100 200)

broker_flags_for() {
    local tname="$1"
    if [ "$tname" = "tls" ]; then
        echo "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key"
    elif [ "$tname" = "quic" ]; then
        echo "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"
    else
        echo ""
    fi
}

run_monitored_single() {
    local experiment="$1"
    local label="$2"
    shift 2
    local bench_args="$*"
    local output_dir="${RESULTS_DIR}/${experiment}"
    mkdir -p "$output_dir"

    start_monitor "${output_dir}/${label}_broker_resources.csv"
    start_client_monitor
    run_bench "$experiment" "$label" "$bench_args"
    stop_client_monitor "${output_dir}/${label}_client_resources.csv"
    stop_monitor "${output_dir}/${label}_broker_resources.csv"
}

for tidx in "${!TRANSPORTS[@]}"; do
    url="${TRANSPORTS[$tidx]}"
    tname="${TRANSPORT_NAMES[$tidx]}"
    flags=$(broker_flags_for "$tname")

    start_broker "$flags"

    for delay in "${DELAYS[@]}"; do
        apply_netem "$delay" 0
        label="${tname}_delay${delay}ms"
        echo "[${EXPERIMENT}] ${label}"

        if [ "$tname" = "quic" ]; then
            for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
                stop_broker
                start_broker "$flags"
                run_monitored_single "$EXPERIMENT" "${label}_run${run}" \
                    "--url ${url} --ca-cert /opt/mqtt-certs/ca.pem --mode connections --concurrency 1 --duration 30"
            done
        else
            run_monitored "$EXPERIMENT" "$label" \
                "--url ${url} --ca-cert /opt/mqtt-certs/ca.pem --mode connections --concurrency 1 --duration 30"
        fi

        clear_netem
    done

    stop_broker
done

echo "experiment ${EXPERIMENT} complete"
