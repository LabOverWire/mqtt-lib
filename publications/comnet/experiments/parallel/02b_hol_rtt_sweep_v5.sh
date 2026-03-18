#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

EXPERIMENT="02b_hol_rtt_sweep"
DELAYS=(10 50 100)
LOSS=1
RUNS_PER_DATAPOINT=15

RESULTS_DIR="${ROOT_DIR}/results-v5"
mkdir -p "$RESULTS_DIR"

BROKER_TLS="--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key"
BROKER_QUIC="--quic-host 0.0.0.0:14567"
CA="--ca-cert /opt/mqtt-certs/ca.pem"

declare -A TRANSPORT_URLS
TRANSPORT_URLS[tcp]="mqtt://${BROKER_IP}:1883"
TRANSPORT_URLS[quic-control]="quic://${BROKER_IP}:14567"
TRANSPORT_URLS[quic-pertopic]="quic://${BROKER_IP}:14567"
TRANSPORT_URLS[quic-perpub]="quic://${BROKER_IP}:14567"

declare -A TRANSPORT_FLAGS
TRANSPORT_FLAGS[tcp]=""
TRANSPORT_FLAGS[quic-control]="--quic-stream-strategy control-only ${CA}"
TRANSPORT_FLAGS[quic-pertopic]="--quic-stream-strategy per-topic ${CA}"
TRANSPORT_FLAGS[quic-perpub]="--quic-stream-strategy per-publish ${CA}"

declare -A BROKER_DELIVERY
BROKER_DELIVERY[tcp]=""
BROKER_DELIVERY[quic-control]="--quic-delivery-strategy control-only"
BROKER_DELIVERY[quic-pertopic]="--quic-delivery-strategy per-topic"
BROKER_DELIVERY[quic-perpub]="--quic-delivery-strategy per-publish"

: "${RTT_TRANSPORTS:=tcp quic-control quic-pertopic quic-perpub}"
read -ra TRANSPORTS <<< "$RTT_TRANSPORTS"

start_monitors() {
    BROKER_MONITOR_PID=$(ssh_broker "nohup bash /opt/mqtt-lib/experiments/monitor/resource_monitor.sh ${BROKER_PID} \
        > /tmp/monitor.csv 2>&1 & echo \$!")
    PUB_MONITOR_PID=$(ssh_pub "nohup bash /opt/mqtt-lib/experiments/monitor/client_monitor.sh \
        > /tmp/client_monitor.csv 2>&1 & echo \$!")
}

stop_monitors() {
    local output_dir="$1"
    local run_label="$2"

    ssh_broker "kill ${BROKER_MONITOR_PID}" 2>/dev/null || true
    ssh_pub "kill ${PUB_MONITOR_PID}" 2>/dev/null || true

    scp -i "$SSH_KEY_PATH" $SSH_OPTS "${SSH_USER}@${BROKER_SSH_IP}:/tmp/monitor.csv" \
        "${output_dir}/${run_label}_broker_resources.csv" 2>/dev/null || true
    scp -i "$SSH_KEY_PATH" $SSH_OPTS "${SSH_USER}@${PUB_IP}:/tmp/client_monitor.csv" \
        "${output_dir}/${run_label}_pub_resources.csv" 2>/dev/null || true

    BROKER_MONITOR_PID=""
    PUB_MONITOR_PID=""
}

collect_traces() {
    local experiment="$1"
    local run_label="$2"
    local remote_dir="$3"
    local output_dir="${RESULTS_DIR}/${experiment}"

    for csv in messages.csv quinn_stats.csv; do
        scp -i "$SSH_KEY_PATH" "${SSH_USER}@${PUB_IP}:${remote_dir}/${csv}" \
            "${output_dir}/${run_label}_${csv}" 2>/dev/null || true
    done
    ssh_pub "rm -rf ${remote_dir}" 2>/dev/null || true
}

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

    for delay in "${DELAYS[@]}"; do
        apply_netem "$delay" "$LOSS"
        label="${tname}_rtt${delay}ms"
        echo "[${EXPERIMENT}] ${label}"

        bench_args="--url ${url} ${flags} --mode hol-blocking --topics 8 --duration 60 --warmup 5 --payload-size 256 --rate 500 --trace-dir /tmp/hol-traces"
        output_dir="${RESULTS_DIR}/${EXPERIMENT}"
        mkdir -p "$output_dir"

        for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
            run_label="${label}_run${run}"
            start_monitors
            run_hol_colocated "$EXPERIMENT" "$run_label" "$bench_args"
            stop_monitors "$output_dir" "$run_label"
            collect_traces "$EXPERIMENT" "$run_label" "/tmp/hol-traces"
            sleep 5
        done

        clear_netem
    done
done

stop_broker
echo "experiment ${EXPERIMENT} v5 complete (group ${GROUP})"
