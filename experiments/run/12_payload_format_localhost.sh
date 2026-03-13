#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_DIR="$(cd "${ROOT_DIR}/.." && pwd)"
RESULTS_DIR="${ROOT_DIR}/results-phase1"
MQTTV5="${REPO_DIR}/target/release/mqttv5"

if [ ! -x "$MQTTV5" ]; then
    echo "release binary not found at ${MQTTV5} — run: cargo build --release -p mqttv5-cli"
    exit 1
fi

EXPERIMENT="12_payload_format_localhost"
FORMATS=(raw json bebytes compressed-json)
SIZES=(64 256 1024 4096)
DURATION=30
WARMUP=5
RUNS=5
QOS=0

BENCH_PORT="${BENCH_PORT:-11883}"
BROKER_PID=""

start_local_broker() {
    echo "starting local broker on port ${BENCH_PORT}..."
    "$MQTTV5" broker --allow-anonymous --host "0.0.0.0:${BENCH_PORT}" --storage-backend memory > /tmp/bench_broker.log 2>&1 &
    BROKER_PID=$!
    sleep 2
    echo "broker pid: ${BROKER_PID}"
}

stop_local_broker() {
    if [ -n "$BROKER_PID" ]; then
        echo "stopping broker (pid ${BROKER_PID})..."
        kill "$BROKER_PID" 2>/dev/null || true
        wait "$BROKER_PID" 2>/dev/null || true
        BROKER_PID=""
    fi
}

trap stop_local_broker EXIT

start_local_broker

output_dir="${RESULTS_DIR}/${EXPERIMENT}"
mkdir -p "$output_dir"

for fmt in "${FORMATS[@]}"; do
    for size in "${SIZES[@]}"; do
        for mode in latency throughput; do
            label="${fmt}_${size}b_${mode}"
            echo "[${EXPERIMENT}] ${label}"
            for run in $(seq 1 "$RUNS"); do
                run_label="${label}_run${run}"
                echo "  run ${run}/${RUNS}..."
                "$MQTTV5" bench \
                    --port "$BENCH_PORT" \
                    --mode "$mode" \
                    --duration "$DURATION" \
                    --warmup "$WARMUP" \
                    --payload-size "$size" \
                    --payload-format "$fmt" \
                    --qos "$QOS" \
                    > "${output_dir}/${run_label}.json" 2>/dev/null
                sleep 2
            done
        done
    done
done

echo "experiment ${EXPERIMENT} complete"
