#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

EXPERIMENT="02_hol_blocking"
LOSSES=(0 1 2 5)
DELAY=25
RUNS_PER_DATAPOINT=15

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

TRANSPORTS=(tcp quic-control quic-pertopic quic-perpub)

TRACE_LOSSES=(0 1)

should_trace() {
    local loss="$1"
    for tl in "${TRACE_LOSSES[@]}"; do
        [[ "$loss" == "$tl" ]] && return 0
    done
    return 1
}

collect_traces() {
    local experiment="$1"
    local run_label="$2"
    local remote_dir="$3"
    local output_dir="${RESULTS_DIR}/${experiment}"

    for csv in messages.csv quinn_stats.csv; do
        scp -i "$SSH_KEY_PATH" "${SSH_USER}@${SUB_IP}:${remote_dir}/${csv}" \
            "${output_dir}/${run_label}_${csv}" 2>/dev/null || true
    done
    ssh_sub "rm -rf ${remote_dir}" 2>/dev/null || true
}

for tname in "${TRANSPORTS[@]}"; do
    url="${TRANSPORT_URLS[$tname]}"
    flags="${TRANSPORT_FLAGS[$tname]}"
    delivery="${BROKER_DELIVERY[$tname]}"

    stop_broker 2>/dev/null || true
    start_broker "${BROKER_TLS} ${BROKER_QUIC} ${delivery}"

    for loss in "${LOSSES[@]}"; do
        apply_netem "$DELAY" "$loss"
        label="${tname}_loss${loss}pct"
        echo "[${EXPERIMENT}] ${label}"

        trace_flag=""
        if should_trace "$loss"; then
            trace_flag="--trace-dir /tmp/hol-traces"
        fi

        bench_args="--url ${url} ${flags} --mode hol-blocking --topics 8 --duration 30 --warmup 5 --payload-size 512 --rate 5000 ${trace_flag}"
        output_dir="${RESULTS_DIR}/${EXPERIMENT}"
        mkdir -p "$output_dir"

        for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
            run_label="${label}_run${run}"
            start_monitors
            run_bench_split "$EXPERIMENT" "$run_label" "$bench_args"
            stop_monitors "$output_dir" "$run_label"

            if should_trace "$loss"; then
                collect_traces "$EXPERIMENT" "$run_label" "/tmp/hol-traces"
            fi

            sleep 5
        done

        clear_netem
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete (group ${GROUP})"
