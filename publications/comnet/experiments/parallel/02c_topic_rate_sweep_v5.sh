#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

EXPERIMENT="02c_topic_rate_sweep"
LOSSES=(1 5)
DELAY=25
RUNS_PER_DATAPOINT=10

RESULTS_DIR="${ROOT_DIR}/results-v5"
OUTPUT_DIR="${RESULTS_DIR}/${EXPERIMENT}"
mkdir -p "$OUTPUT_DIR"

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

: "${V3_TRANSPORTS:=tcp quic-control quic-pertopic quic-perpub}"
read -ra TRANSPORTS <<< "$V3_TRANSPORTS"

CELLS=(
    "2 500" "4 500" "16 500" "32 500"
    "8 125" "8 250" "8 1000" "8 2000"
)

start_monitors() {
    BROKER_MONITOR_PID=$(ssh_broker "nohup bash /opt/mqtt-lib/experiments/monitor/resource_monitor.sh ${BROKER_PID} \
        > /tmp/monitor.csv 2>&1 & echo \$!") || BROKER_MONITOR_PID=""
    PUB_MONITOR_PID=$(ssh_pub "nohup bash /opt/mqtt-lib/experiments/monitor/client_monitor.sh \
        > /tmp/client_monitor.csv 2>&1 & echo \$!") || PUB_MONITOR_PID=""
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
    for csv in messages.csv quinn_stats.csv; do
        scp -i "$SSH_KEY_PATH" "${SSH_USER}@${PUB_IP}:${remote_dir}/${csv}" \
            "${OUTPUT_DIR}/${run_label}_${csv}" 2>/dev/null || true
    done
    ssh_pub "rm -rf ${remote_dir}" 2>/dev/null || true
}

run_hol_colocated() {
    local label="$1"
    shift
    local bench_args="$*"
    echo "  running (co-located): ${label}"
    ssh_pub "ulimit -n 65536; mqttv5 bench ${bench_args}" \
        > "${OUTPUT_DIR}/${label}.json" 2>/dev/null || true
    warn_if_empty "${OUTPUT_DIR}/${label}.json"
    echo "  saved: ${OUTPUT_DIR}/${label}.json"
}

for tname in "${TRANSPORTS[@]}"; do
    url="${TRANSPORT_URLS[$tname]}"
    flags="${TRANSPORT_FLAGS[$tname]}"
    delivery="${BROKER_DELIVERY[$tname]}"

    stop_broker 2>/dev/null || true
    if ! start_broker "${BROKER_TLS} ${BROKER_QUIC} ${delivery}"; then
        echo "WARN: broker start failed for ${tname}, skipping transport" >&2
        continue
    fi

    for cell in "${CELLS[@]}"; do
        read -r topics rate <<< "$cell"
        for loss in "${LOSSES[@]}"; do
            label="${tname}_t${topics}_r${rate}_loss${loss}pct"

            done_runs=0
            for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
                f="${OUTPUT_DIR}/${label}_run${run}.json"
                if [ -s "$f" ] && [ "$(wc -c < "$f")" -gt 500 ]; then
                    done_runs=$((done_runs + 1))
                fi
            done
            if [ "$done_runs" = "$RUNS_PER_DATAPOINT" ]; then
                echo "[${EXPERIMENT}] ${label} complete (${done_runs}/${RUNS_PER_DATAPOINT}), skipping"
                continue
            fi

            apply_netem "$DELAY" "$loss"
            echo "[${EXPERIMENT}] ${label} (${done_runs}/${RUNS_PER_DATAPOINT} done)"

            bench_args="--url ${url} ${flags} --mode hol-blocking --topics ${topics} --duration 60 --warmup 5 --payload-size 256 --rate ${rate} --trace-dir /tmp/hol-traces"

            for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
                run_label="${label}_run${run}"
                existing="${OUTPUT_DIR}/${run_label}.json"
                if [ -s "$existing" ] && [ "$(wc -c < "$existing")" -gt 500 ]; then
                    echo "  skip (exists): ${run_label}"
                    continue
                fi
                if [ "$BROKER_FRESH" = "1" ]; then
                    BROKER_FRESH=0
                elif ! restart_broker; then
                    echo "WARN: broker restart failed, skipping ${run_label}" >&2
                    continue
                fi
                start_monitors
                run_hol_colocated "$run_label" "$bench_args"
                stop_monitors "$OUTPUT_DIR" "$run_label"
                collect_traces "$run_label" "/tmp/hol-traces"
                sleep 5
            done

            clear_netem
        done
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete (group ${GROUP})"
