#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

EXPERIMENT="02e_hol_qos1"
DELAY=25
LOSS=1
RUNS_PER_DATAPOINT=15

RESULTS_DIR="${ROOT_DIR}/results-v5"
mkdir -p "$RESULTS_DIR"

BROKER_TLS="--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key"
BROKER_QUIC="--quic-host 0.0.0.0:14567"
CA="--ca-cert /opt/mqtt-certs/ca.pem"

declare -A TRANSPORT_URLS
TRANSPORT_URLS[tcp]="mqtt://${BROKER_IP}:1883"
TRANSPORT_URLS[quic-pertopic]="quic://${BROKER_IP}:14567"

declare -A TRANSPORT_FLAGS
TRANSPORT_FLAGS[tcp]=""
TRANSPORT_FLAGS[quic-pertopic]="--quic-stream-strategy per-topic ${CA}"

declare -A BROKER_DELIVERY
BROKER_DELIVERY[tcp]=""
BROKER_DELIVERY[quic-pertopic]="--quic-delivery-strategy per-topic"

TRANSPORTS=(tcp quic-pertopic)

run_hol_colocated() {
    local experiment="$1"
    local label="$2"
    shift 2
    local bench_args="$*"
    local output_dir="${RESULTS_DIR}/${experiment}"
    mkdir -p "$output_dir"

    echo "  running (co-located): ${label}"
    ssh_pub "ulimit -n 65536; mqttv5 bench ${bench_args}" \
        > "${output_dir}/${label}.json" 2>/dev/null || true
    echo "  saved: ${output_dir}/${label}.json"
}

for tname in "${TRANSPORTS[@]}"; do
    url="${TRANSPORT_URLS[$tname]}"
    flags="${TRANSPORT_FLAGS[$tname]}"
    delivery="${BROKER_DELIVERY[$tname]}"

    stop_broker 2>/dev/null || true
    start_broker "${BROKER_TLS} ${BROKER_QUIC} ${delivery}"

    apply_netem "$DELAY" "$LOSS"
    label="${tname}_qos1"
    echo "[${EXPERIMENT}] ${label}"

    bench_args="--url ${url} ${flags} --mode hol-blocking --topics 8 --duration 60 --warmup 5 --payload-size 256 --rate 500 --qos 1 --trace-dir /tmp/hol-traces"
    output_dir="${RESULTS_DIR}/${EXPERIMENT}"
    mkdir -p "$output_dir"

    for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
        run_label="${label}_run${run}"
        run_hol_colocated "$EXPERIMENT" "$run_label" "$bench_args"
        sleep 3
    done

    clear_netem
done

stop_broker
echo "experiment ${EXPERIMENT} v5 complete (group ${GROUP})"
