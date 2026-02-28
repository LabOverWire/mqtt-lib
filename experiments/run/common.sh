#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
source "${ROOT_DIR}/setup/config.env"

: "${BROKER_IP:?Set BROKER_IP in config.env}"
: "${BROKER_SSH_IP:=${BROKER_IP}}"
: "${CLIENT_IP:?Set CLIENT_IP in config.env}"
: "${SSH_KEY_PATH:=$HOME/.ssh/id_ed25519}"
: "${RUNS_PER_DATAPOINT:=5}"

SSH_USER="${SSH_USER:-bench}"

RESULTS_DIR="${ROOT_DIR}/results"
mkdir -p "$RESULTS_DIR"

ssh_broker() { ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no "${SSH_USER}@${BROKER_SSH_IP}" "$@"; }
ssh_client() { ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no "${SSH_USER}@${CLIENT_IP}" "$@"; }

BROKER_PID=""

start_broker() {
    local extra_flags="${1:-}"
    echo "starting broker on ${BROKER_IP}..."
    BROKER_PID=$(ssh_broker "ulimit -n 65536; nohup mqttv5 broker --allow-anonymous --host 0.0.0.0:1883 --storage-backend memory --max-clients 50000 \
        ${extra_flags} > /tmp/broker.log 2>&1 & echo \$!")
    sleep 2
    echo "broker pid: ${BROKER_PID}"
}

stop_broker() {
    echo "stopping broker..."
    ssh_broker "pkill -f 'mqttv5 broker'" 2>/dev/null || true
    BROKER_PID=""
    sleep 1
}

apply_netem() {
    local delay_ms="$1"
    local loss_pct="${2:-0}"
    ssh_client "sudo bash /opt/mqtt-lib/experiments/netem/apply.sh ${delay_ms} ${loss_pct}"
}

clear_netem() {
    ssh_client "sudo bash /opt/mqtt-lib/experiments/netem/clear.sh"
}

MONITOR_PID=""

start_monitor() {
    local output_file="$1"
    MONITOR_PID=$(ssh_broker "nohup bash /opt/mqtt-lib/experiments/monitor/resource_monitor.sh ${BROKER_PID} \
        > /tmp/monitor.csv 2>&1 & echo \$!")
    echo "monitor pid: ${MONITOR_PID}"
}

stop_monitor() {
    local output_file="$1"
    ssh_broker "kill ${MONITOR_PID}" 2>/dev/null || true
    scp -i "$SSH_KEY_PATH" "${SSH_USER}@${BROKER_SSH_IP}:/tmp/monitor.csv" "$output_file"
    MONITOR_PID=""
}

CLIENT_MONITOR_PID=""

start_client_monitor() {
    CLIENT_MONITOR_PID=$(ssh_client "nohup bash /opt/mqtt-lib/experiments/monitor/client_monitor.sh \
        > /tmp/client_monitor.csv 2>&1 & echo \$!")
    echo "client monitor pid: ${CLIENT_MONITOR_PID}"
}

stop_client_monitor() {
    local output_file="$1"
    ssh_client "kill ${CLIENT_MONITOR_PID}" 2>/dev/null || true
    scp -i "$SSH_KEY_PATH" "${SSH_USER}@${CLIENT_IP}:/tmp/client_monitor.csv" "$output_file"
    CLIENT_MONITOR_PID=""
}

run_bench() {
    local experiment="$1"
    local label="$2"
    shift 2
    local bench_args="$*"
    local output_dir="${RESULTS_DIR}/${experiment}"
    mkdir -p "$output_dir"
    local output_file="${output_dir}/${label}.json"

    echo "  running: mqttv5 bench ${bench_args}"
    ssh_client "ulimit -n 65536; mqttv5 bench ${bench_args}" > "$output_file" 2>/dev/null
    echo "  saved: ${output_file}"
}

run_repeated() {
    local experiment="$1"
    local label="$2"
    shift 2
    local bench_args="$*"

    for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
        run_bench "$experiment" "${label}_run${run}" "$bench_args"
        sleep 5
    done
}

run_monitored() {
    local experiment="$1"
    local label="$2"
    shift 2
    local bench_args="$*"
    local output_dir="${RESULTS_DIR}/${experiment}"
    mkdir -p "$output_dir"

    for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
        local run_label="${label}_run${run}"
        start_monitor "${output_dir}/${run_label}_broker_resources.csv"
        start_client_monitor
        run_bench "$experiment" "$run_label" "$bench_args"
        stop_client_monitor "${output_dir}/${run_label}_client_resources.csv"
        stop_monitor "${output_dir}/${run_label}_broker_resources.csv"
        sleep 5
    done
}
