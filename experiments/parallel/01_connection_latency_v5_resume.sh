#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

EXPERIMENT="01_connection_latency"
RUNS_PER_DATAPOINT=15

RESULTS_DIR="${ROOT_DIR}/results-v5"
mkdir -p "$RESULTS_DIR"

start_monitors() {
    BROKER_MONITOR_PID=$(ssh_broker "nohup bash /opt/mqtt-lib/experiments/monitor/resource_monitor.sh ${BROKER_PID} \
        > /tmp/monitor.csv 2>&1 & echo \$!" || echo "0")
    PUB_MONITOR_PID=$(ssh_pub "nohup bash /opt/mqtt-lib/experiments/monitor/client_monitor.sh \
        > /tmp/client_monitor.csv 2>&1 & echo \$!" || echo "0")
}

stop_monitors() {
    local output_dir="$1"
    local run_label="$2"
    ssh_broker "kill ${BROKER_MONITOR_PID}" 2>/dev/null || true
    ssh_pub "kill ${PUB_MONITOR_PID}" 2>/dev/null || true
    scp -i "$SSH_KEY_PATH" $SSH_OPTS "${SSH_USER}@${BROKER_SSH_IP}:/tmp/monitor.csv" \
        "${output_dir}/${run_label}_broker_resources.csv" || true
    scp -i "$SSH_KEY_PATH" $SSH_OPTS "${SSH_USER}@${PUB_IP}:/tmp/client_monitor.csv" \
        "${output_dir}/${run_label}_pub_resources.csv" || true
    BROKER_MONITOR_PID=""
    PUB_MONITOR_PID=""
}

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

    start_monitors
    run_bench_pub_only "$experiment" "$label" "$bench_args"
    stop_monitors "$output_dir" "$label"
}

echo "=== Resuming Exp 1: tcp_delay25ms from run 7 ==="

flags=$(broker_flags_for "tcp")
start_broker "$flags"

apply_netem 25 0
echo "[${EXPERIMENT}] tcp_delay25ms (runs 7-15)"
for run in $(seq 7 "$RUNS_PER_DATAPOINT"); do
    run_label="tcp_delay25ms_run${run}"
    start_monitors
    run_bench_pub_only "$EXPERIMENT" "$run_label" \
        "--url mqtt://${BROKER_IP}:1883 --ca-cert /opt/mqtt-certs/ca.pem --mode connections --concurrency 1 --duration 30"
    stop_monitors "${RESULTS_DIR}/${EXPERIMENT}" "$run_label"
    sleep 5
done
clear_netem

for delay in 50 100 200; do
    apply_netem "$delay" 0
    label="tcp_delay${delay}ms"
    echo "[${EXPERIMENT}] ${label}"
    run_monitored_pub_only "$EXPERIMENT" "$label" \
        "--url mqtt://${BROKER_IP}:1883 --ca-cert /opt/mqtt-certs/ca.pem --mode connections --concurrency 1 --duration 30"
    clear_netem
done

stop_broker

echo "=== TLS transport ==="
flags=$(broker_flags_for "tls")
start_broker "$flags"

for delay in 0 25 50 100 200; do
    apply_netem "$delay" 0
    label="tls_delay${delay}ms"
    echo "[${EXPERIMENT}] ${label}"
    run_monitored_pub_only "$EXPERIMENT" "$label" \
        "--url mqtts://${BROKER_IP}:8883 --ca-cert /opt/mqtt-certs/ca.pem --mode connections --concurrency 1 --duration 30"
    clear_netem
done

stop_broker

echo "=== QUIC transport ==="
flags=$(broker_flags_for "quic")

for delay in 0 25 50 100 200; do
    apply_netem "$delay" 0
    label="quic_delay${delay}ms"
    echo "[${EXPERIMENT}] ${label}"
    for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
        stop_broker 2>/dev/null || true
        start_broker "$flags"
        run_monitored_single "$EXPERIMENT" "${label}_run${run}" \
            "--url quic://${BROKER_IP}:14567 --ca-cert /opt/mqtt-certs/ca.pem --mode connections --concurrency 1 --duration 30"
    done
    clear_netem
done

stop_broker

echo "experiment ${EXPERIMENT} v5 RESUME COMPLETE (group ${GROUP})"
