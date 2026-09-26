#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

EXPERIMENT="04b_stream_limit_sweep"
STRATEGIES=("control-only" "per-topic" "per-publish")
STREAM_LIMITS=(100 250 1000)
TOPICS=8
DELAY=25
LOSS=2
RUNS_PER_DATAPOINT="${RUNS_PER_DATAPOINT:-5}"

RESULTS_DIR="${ROOT_DIR}/results-v5"
OUTPUT_DIR="${RESULTS_DIR}/${EXPERIMENT}"
mkdir -p "$OUTPUT_DIR"

BROKER_TLS="--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key"
CA="--ca-cert /opt/mqtt-certs/ca.pem"

run_hol_colocated() {
    local label="$1"
    shift
    echo "  running (co-located): ${label}"
    ssh_pub "ulimit -n 65536; mqttv5 bench $*" \
        > "${OUTPUT_DIR}/${label}.json" 2>/dev/null || true
    warn_if_empty "${OUTPUT_DIR}/${label}.json"
}

for limit in "${STREAM_LIMITS[@]}"; do
    for strategy in "${STRATEGIES[@]}"; do
        clear_netem 2>/dev/null || true
        stop_broker 2>/dev/null || true
        if ! start_broker "${BROKER_TLS} --quic-host 0.0.0.0:14567 --quic-delivery-strategy ${strategy} --quic-max-streams ${limit}"; then
            echo "WARN: broker start failed for ${strategy} limit ${limit}, skipping" >&2
            continue
        fi

        apply_netem "$DELAY" "$LOSS"
        label="${strategy}_limit${limit}_throughput"
        echo "[${EXPERIMENT}] ${label}"
        run_monitored_split "$EXPERIMENT" "$label" \
            "--url quic://${BROKER_IP}:14567 ${CA} --quic-stream-strategy ${strategy} --quic-max-streams ${limit} --mode throughput --duration 60 --warmup 5 --payload-size 256 --publishers 1 --topics ${TOPICS} --subscribers 1 --inflight 64"

        apply_netem "$DELAY" 5
        hol_label="${strategy}_limit${limit}_hol_r2000_loss5pct"
        echo "[${EXPERIMENT}] ${hol_label}"
        for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
            run_label="${hol_label}_run${run}"
            if [ -s "${OUTPUT_DIR}/${run_label}.json" ]; then
                echo "  skip (exists): ${run_label}"
                continue
            fi
            if [ "$BROKER_FRESH" = "1" ]; then
                BROKER_FRESH=0
            elif ! restart_broker; then
                echo "WARN: broker restart failed, skipping ${run_label}" >&2
                continue
            fi
            run_hol_colocated "$run_label" \
                "--url quic://${BROKER_IP}:14567 ${CA} --quic-stream-strategy ${strategy} --quic-max-streams ${limit} --mode hol-blocking --topics ${TOPICS} --duration 60 --warmup 5 --payload-size 256 --rate 2000"
            sleep 5
        done
        clear_netem
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete (group ${GROUP})"
