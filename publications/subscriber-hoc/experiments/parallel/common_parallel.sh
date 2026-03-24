#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

: "${GROUP:?Set GROUP=1|2|3 before sourcing common_parallel.sh}"
source "${SCRIPT_DIR}/group${GROUP}.env"

: "${BROKER_IP:?Set BROKER_IP in group${GROUP}.env}"
: "${BROKER_SSH_IP:=${BROKER_IP}}"
: "${PUB_IP:?Set PUB_IP in group${GROUP}.env}"
: "${SUB_IP:?Set SUB_IP in group${GROUP}.env}"
: "${SSH_KEY_PATH:=$HOME/.ssh/id_ed25519}"
: "${RUNS_PER_DATAPOINT:=10}"

SSH_USER="${SSH_USER:-bench}"
RESULTS_DIR="${ROOT_DIR}/results"
mkdir -p "$RESULTS_DIR"

SSH_OPTS="-o StrictHostKeyChecking=no -o ServerAliveInterval=30 -o ServerAliveCountMax=10"
SUB_SSH_OPTS="$SSH_OPTS"
if [ -n "${SUB_PROXY:-}" ]; then
    SUB_SSH_OPTS="$SSH_OPTS -o ProxyJump=${SSH_USER}@${SUB_PROXY}"
fi
ssh_broker() { ssh -i "$SSH_KEY_PATH" $SSH_OPTS "${SSH_USER}@${BROKER_SSH_IP}" "$@"; }
ssh_pub()    { ssh -i "$SSH_KEY_PATH" $SSH_OPTS "${SSH_USER}@${PUB_IP}" "$@"; }
ssh_sub()    { ssh -i "$SSH_KEY_PATH" $SUB_SSH_OPTS "${SSH_USER}@${SUB_IP}" "$@"; }

scp_from_sub() {
    local remote_path="$1"
    local local_path="$2"
    if [ -n "${SUB_PROXY:-}" ]; then
        scp -i "$SSH_KEY_PATH" -o ProxyJump="${SSH_USER}@${SUB_PROXY}" \
            "${SSH_USER}@${SUB_IP}:${remote_path}" "$local_path"
    else
        scp -i "$SSH_KEY_PATH" "${SSH_USER}@${SUB_IP}:${remote_path}" "$local_path"
    fi
}

BROKER_PID=""

start_broker() {
    local extra_flags="${1:-}"
    echo "starting broker on ${BROKER_IP} (group ${GROUP})..."
    BROKER_PID=$(ssh_broker "ulimit -n 65536; nohup mqttv5 broker --allow-anonymous --host 0.0.0.0:1883 --storage-backend memory --max-clients 50000 \
        ${extra_flags} > /tmp/broker.log 2>&1 & echo \$!")
    sleep 2
    echo "broker pid: ${BROKER_PID}"
}

stop_broker() {
    echo "stopping broker (group ${GROUP})..."
    ssh_broker "pkill -f 'mqttv5 broker'" 2>/dev/null || true
    BROKER_PID=""
    sleep 1
}

apply_netem() {
    local delay_ms="$1"
    local loss_pct="${2:-0}"
    ssh_broker "sudo bash /opt/mqtt-lib/experiments/netem/apply.sh ${delay_ms} ${loss_pct}"
}

clear_netem() {
    ssh_broker "sudo bash /opt/mqtt-lib/experiments/netem/clear.sh"
}

BROKER_MONITOR_PID=""
PUB_MONITOR_PID=""
SUB_MONITOR_PID=""

start_monitors() {
    BROKER_MONITOR_PID=$(ssh_broker "nohup bash /opt/mqtt-lib/experiments/monitor/resource_monitor.sh ${BROKER_PID} \
        > /tmp/monitor.csv 2>&1 & echo \$!")
    PUB_MONITOR_PID=$(ssh_pub "nohup bash /opt/mqtt-lib/experiments/monitor/client_monitor.sh \
        > /tmp/client_monitor.csv 2>&1 & echo \$!")
    SUB_MONITOR_PID=$(ssh_sub "nohup bash /opt/mqtt-lib/experiments/monitor/client_monitor.sh \
        > /tmp/client_monitor.csv 2>&1 & echo \$!")
}

stop_monitors() {
    local output_dir="$1"
    local run_label="$2"

    ssh_broker "kill ${BROKER_MONITOR_PID}" 2>/dev/null || true
    ssh_pub "kill ${PUB_MONITOR_PID}" 2>/dev/null || true
    ssh_sub "kill ${SUB_MONITOR_PID}" 2>/dev/null || true

    scp -i "$SSH_KEY_PATH" "${SSH_USER}@${BROKER_SSH_IP}:/tmp/monitor.csv" \
        "${output_dir}/${run_label}_broker_resources.csv"
    scp -i "$SSH_KEY_PATH" "${SSH_USER}@${PUB_IP}:/tmp/client_monitor.csv" \
        "${output_dir}/${run_label}_pub_resources.csv"
    scp_from_sub "/tmp/client_monitor.csv" "${output_dir}/${run_label}_sub_resources.csv"

    BROKER_MONITOR_PID=""
    PUB_MONITOR_PID=""
    SUB_MONITOR_PID=""
}

run_bench_pub_only() {
    local experiment="$1"
    local label="$2"
    shift 2
    local bench_args="$*"
    local output_dir="${RESULTS_DIR}/${experiment}"
    mkdir -p "$output_dir"

    echo "  running (pub-only): mqttv5 bench ${bench_args}"
    ssh_pub "ulimit -n 65536; mqttv5 bench ${bench_args}" > "${output_dir}/${label}.json" 2>/dev/null || true
    echo "  saved: ${output_dir}/${label}.json"
}

run_bench_split() {
    local experiment="$1"
    local label="$2"
    shift 2
    local bench_args="$*"
    local output_dir="${RESULTS_DIR}/${experiment}"
    mkdir -p "$output_dir"
    local sub_duration
    sub_duration=$(echo "$bench_args" | sed -n 's/.*--duration \([0-9]*\).*/\1/p')
    local sub_extra=10
    local sub_total=$((sub_duration + sub_extra))

    echo "  running (split): ${label}"

    local sub_args
    sub_args=$(echo "$bench_args" | sed "s/--duration ${sub_duration}/--duration ${sub_total}/")
    sub_args=$(echo "$sub_args" | sed 's/--publishers [0-9]*/--publishers 0/')
    if ! echo "$sub_args" | grep -q -- '--publishers'; then
        sub_args="${sub_args} --publishers 0"
    fi

    local pub_args
    pub_args=$(echo "$bench_args" | sed 's/--subscribers [0-9]*/--subscribers 0/')
    if ! echo "$pub_args" | grep -q -- '--subscribers'; then
        pub_args="${pub_args} --subscribers 0"
    fi

    ssh_sub "rm -f /tmp/sub_bench.json; ulimit -n 65536; nohup mqttv5 bench ${sub_args} \
        > /tmp/sub_bench.json 2>/dev/null &"
    sleep 2

    ssh_pub "ulimit -n 65536; mqttv5 bench ${pub_args}" \
        > "${output_dir}/${label}_pub.json" 2>/dev/null || true

    local waited=0
    while [ "$waited" -lt 60 ]; do
        local size
        size=$(ssh_sub "stat -c%s /tmp/sub_bench.json 2>/dev/null || echo 0")
        if [ "$size" -gt 0 ]; then
            break
        fi
        sleep 2
        waited=$((waited + 2))
    done

    scp_from_sub "/tmp/sub_bench.json" "${output_dir}/${label}.json"
    echo "  saved: ${output_dir}/${label}.json"
}

run_monitored_pub_only() {
    local experiment="$1"
    local label="$2"
    shift 2
    local bench_args="$*"
    local output_dir="${RESULTS_DIR}/${experiment}"
    mkdir -p "$output_dir"

    for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
        local run_label="${label}_run${run}"
        start_monitors
        run_bench_pub_only "$experiment" "$run_label" "$bench_args"
        stop_monitors "$output_dir" "$run_label"
        sleep 5
    done
}

run_monitored_split() {
    local experiment="$1"
    local label="$2"
    shift 2
    local bench_args="$*"
    local output_dir="${RESULTS_DIR}/${experiment}"
    mkdir -p "$output_dir"

    for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
        local run_label="${label}_run${run}"
        start_monitors
        run_bench_split "$experiment" "$run_label" "$bench_args"
        stop_monitors "$output_dir" "$run_label"
        sleep 5
    done
}
