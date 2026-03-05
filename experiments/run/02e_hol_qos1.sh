#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="02e_hol_qos1"
DELAY=25
LOSS=1

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

collect_traces() {
    local experiment="$1"
    local run_label="$2"
    local remote_dir="$3"
    local output_dir="${RESULTS_DIR}/${experiment}"

    for csv in messages.csv quinn_stats.csv; do
        scp -i "$SSH_KEY_PATH" "${SSH_USER}@${CLIENT_IP}:${remote_dir}/${csv}" \
            "${output_dir}/${run_label}_${csv}" 2>/dev/null || true
    done
    ssh_client "rm -rf ${remote_dir}" 2>/dev/null || true
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

    bench_args="--url ${url} ${flags} --mode hol-blocking --topics 8 --duration 30 --warmup 5 --payload-size 512 --rate 5000 --qos 1 --trace-dir /tmp/hol-traces"
    output_dir="${RESULTS_DIR}/${EXPERIMENT}"
    mkdir -p "$output_dir"

    for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
        run_label="${label}_run${run}"
        start_monitor "${output_dir}/${run_label}_broker_resources.csv"
        start_client_monitor
        run_bench "$EXPERIMENT" "$run_label" "$bench_args"
        stop_client_monitor "${output_dir}/${run_label}_client_resources.csv"
        stop_monitor "${output_dir}/${run_label}_broker_resources.csv"
        collect_traces "$EXPERIMENT" "$run_label" "/tmp/hol-traces"
        sleep 5
    done

    clear_netem
done

stop_broker
echo "experiment ${EXPERIMENT} (QoS 1 HOL blocking) complete"
