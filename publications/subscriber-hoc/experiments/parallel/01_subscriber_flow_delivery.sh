#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

EXPERIMENT="01_subscriber_flow_delivery"
LOSSES=(0 1 2 5)
DELAY=25
RUNS_PER_DATAPOINT=15

RESULTS_DIR="${ROOT_DIR}/results"
mkdir -p "$RESULTS_DIR"

BROKER_TLS="--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key"
BROKER_QUIC="--quic-host 0.0.0.0:14567"
CA="--ca-cert /opt/mqtt-certs/ca.pem"

PUB_URL="mqtt://${BROKER_IP}:1883"
SUB_URL="quic://${BROKER_IP}:14567"

declare -A SUB_MODE_FLAGS
SUB_MODE_FLAGS[control]="--quic-stream-strategy control-only ${CA}"
SUB_MODE_FLAGS[per-topic-flow]="--quic-stream-strategy per-topic --quic-flow-headers ${CA}"

declare -A BROKER_DELIVERY
BROKER_DELIVERY[control]="--quic-delivery-strategy control-only"
BROKER_DELIVERY[per-topic-flow]="--quic-delivery-strategy per-topic"

: "${SUB_MODES:=control per-topic-flow}"
read -ra MODES <<< "$SUB_MODES"

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
    local run_label="$1"
    local remote_dir="$2"
    local output_dir="${RESULTS_DIR}/${EXPERIMENT}"

    for csv in messages.csv quinn_stats.csv; do
        scp -i "$SSH_KEY_PATH" $SSH_OPTS "${SSH_USER}@${PUB_IP}:${remote_dir}/${csv}" \
            "${output_dir}/${run_label}_${csv}" 2>/dev/null || true
    done
    ssh_pub "rm -rf ${remote_dir}" 2>/dev/null || true
}

run_hol_colocated() {
    local label="$1"
    shift
    local bench_args="$*"
    local output_dir="${RESULTS_DIR}/${EXPERIMENT}"
    mkdir -p "$output_dir"

    echo "  running (co-located): ${label}"
    ssh_pub "ulimit -n 65536; mqttv5 bench ${bench_args}" \
        > "${output_dir}/${label}.json" 2>/dev/null || true
    echo "  saved: ${output_dir}/${label}.json"
}

for mode in "${MODES[@]}"; do
    sub_flags="${SUB_MODE_FLAGS[$mode]}"
    delivery="${BROKER_DELIVERY[$mode]}"

    stop_broker 2>/dev/null || true
    start_broker "${BROKER_TLS} ${BROKER_QUIC} ${delivery}"

    for loss in "${LOSSES[@]}"; do
        apply_netem "$DELAY" "$loss"
        label="${mode}_loss${loss}pct"
        echo "[${EXPERIMENT}] ${label}"

        bench_args="--pub-url ${PUB_URL} --url ${SUB_URL} ${sub_flags} --mode hol-blocking --topics 8 --duration 60 --warmup 5 --payload-size 256 --rate 500 --trace-dir /tmp/hol-traces"
        output_dir="${RESULTS_DIR}/${EXPERIMENT}"
        mkdir -p "$output_dir"

        for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
            run_label="${label}_run${run}"
            start_monitors
            run_hol_colocated "$run_label" "$bench_args"
            stop_monitors "$output_dir" "$run_label"
            collect_traces "$run_label" "/tmp/hol-traces"
            sleep 5
        done

        clear_netem
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete (group ${GROUP})"
