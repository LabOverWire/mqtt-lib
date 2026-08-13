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
RESULTS_DIR="${ROOT_DIR}/results_v2"
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
BROKER_FLAGS=""
BROKER_FRESH=0

start_broker() {
    local extra_flags="${1:-}"
    BROKER_FLAGS="$extra_flags"
    local attempt
    for attempt in 1 2 3; do
        echo "starting broker on ${BROKER_IP} (group ${GROUP}) [attempt ${attempt}]..."
        BROKER_PID=$(ssh_broker "ulimit -n 65536; nohup mqttv5 broker --allow-anonymous --host 0.0.0.0:1883 --storage-backend memory --max-clients 50000 \
            ${extra_flags} > /tmp/broker.log 2>&1 & echo \$!") || BROKER_PID=""
        sleep 2
        if [ -n "${BROKER_PID}" ] && ssh_broker "kill -0 ${BROKER_PID}" 2>/dev/null; then
            echo "broker pid: ${BROKER_PID}"
            BROKER_FRESH=1
            return 0
        fi
        echo "WARN: broker not running after start attempt ${attempt} (see /tmp/broker.log on ${BROKER_IP})" >&2
        ssh_broker "pkill -f '[m]qttv5 broker'" 2>/dev/null || true
        sleep 1
    done
    echo "ERROR: broker failed to start after 3 attempts on ${BROKER_IP}" >&2
    return 1
}

stop_broker() {
    echo "stopping broker (group ${GROUP})..."
    ssh_broker "pkill -f '[m]qttv5 broker' 2>/dev/null; for _ in \$(seq 1 20); do pgrep -f '[m]qttv5 broker' >/dev/null 2>&1 || break; sleep 0.5; done" || true
    BROKER_PID=""
}

CUR_NETEM_DELAY=""
CUR_NETEM_LOSS=""

apply_netem() {
    local delay_ms="$1"
    local loss_pct="${2:-0}"
    CUR_NETEM_DELAY="$delay_ms"
    CUR_NETEM_LOSS="$loss_pct"
    ssh_broker "sudo bash /opt/mqtt-lib/experiments/netem/apply.sh ${delay_ms} ${loss_pct}"
}

clear_netem() {
    CUR_NETEM_DELAY=""
    CUR_NETEM_LOSS=""
    ssh_broker "sudo bash /opt/mqtt-lib/experiments/netem/clear.sh"
}

restore_netem() {
    if [ -n "$CUR_NETEM_DELAY" ]; then
        ssh_broker "sudo bash /opt/mqtt-lib/experiments/netem/apply.sh ${CUR_NETEM_DELAY} ${CUR_NETEM_LOSS}" 2>/dev/null || true
    fi
}

restart_broker() {
    if [ -n "$CUR_NETEM_DELAY" ]; then
        ssh_broker "sudo bash /opt/mqtt-lib/experiments/netem/clear.sh" 2>/dev/null || true
    fi
    stop_broker
    if ! start_broker "$BROKER_FLAGS"; then
        restore_netem
        return 1
    fi
    restore_netem
    BROKER_FRESH=0
}

BROKER_MONITOR_PID=""
PUB_MONITOR_PID=""
SUB_MONITOR_PID=""

start_monitors() {
    BROKER_MONITOR_PID=$(ssh_broker "nohup bash /opt/mqtt-lib/experiments/monitor/resource_monitor.sh ${BROKER_PID} \
        > /tmp/monitor.csv 2>&1 & echo \$!") || BROKER_MONITOR_PID=""
    PUB_MONITOR_PID=$(ssh_pub "nohup bash /opt/mqtt-lib/experiments/monitor/client_monitor.sh \
        > /tmp/client_monitor.csv 2>&1 & echo \$!") || PUB_MONITOR_PID=""
    SUB_MONITOR_PID=$(ssh_sub "nohup bash /opt/mqtt-lib/experiments/monitor/client_monitor.sh \
        > /tmp/client_monitor.csv 2>&1 & echo \$!") || SUB_MONITOR_PID=""
}

stop_monitors() {
    local output_dir="$1"
    local run_label="$2"

    ssh_broker "kill ${BROKER_MONITOR_PID}" 2>/dev/null || true
    ssh_pub "kill ${PUB_MONITOR_PID}" 2>/dev/null || true
    ssh_sub "kill ${SUB_MONITOR_PID}" 2>/dev/null || true

    scp -i "$SSH_KEY_PATH" "${SSH_USER}@${BROKER_SSH_IP}:/tmp/monitor.csv" \
        "${output_dir}/${run_label}_broker_resources.csv" || true
    scp -i "$SSH_KEY_PATH" "${SSH_USER}@${PUB_IP}:/tmp/client_monitor.csv" \
        "${output_dir}/${run_label}_pub_resources.csv" || true
    scp_from_sub "/tmp/client_monitor.csv" "${output_dir}/${run_label}_sub_resources.csv" || true

    BROKER_MONITOR_PID=""
    PUB_MONITOR_PID=""
    SUB_MONITOR_PID=""
}

warn_if_empty() {
    if [ ! -s "$1" ]; then
        echo "WARN: empty result $1 (broker down or bench failed)" >&2
    fi
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
    warn_if_empty "${output_dir}/${label}.json"
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

    scp_from_sub "/tmp/sub_bench.json" "${output_dir}/${label}.json" || true
    warn_if_empty "${output_dir}/${label}.json"
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
        if [ "$BROKER_FRESH" = "1" ]; then
            BROKER_FRESH=0
        elif ! restart_broker; then
            echo "WARN: broker restart failed, skipping ${run_label}" >&2
            continue
        fi
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
        if [ "$BROKER_FRESH" = "1" ]; then
            BROKER_FRESH=0
        elif ! restart_broker; then
            echo "WARN: broker restart failed, skipping ${run_label}" >&2
            continue
        fi
        start_monitors
        run_bench_split "$experiment" "$run_label" "$bench_args"
        stop_monitors "$output_dir" "$run_label"
        sleep 5
    done
}
