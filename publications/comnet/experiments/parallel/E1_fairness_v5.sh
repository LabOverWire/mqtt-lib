#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

: "${PUB_INTERNAL_IP:?Set PUB_INTERNAL_IP (bottlenecked subnet address of the sub/pub host) in group${GROUP}.env}"

EXPERIMENT="E1_fairness"
DELAY=25
RATE_MBIT="${RATE_MBIT:-50}"
QUEUE_PKTS="${QUEUE_PKTS:-1000}"
read -ra LOSSES <<< "${LOSSES_OVERRIDE:-0 1}"
NTOPICS="${NTOPICS:-8}"
OFFERED_RATE="${OFFERED_RATE:-40000}"
DURATION="${DURATION:-60}"
WARMUP="${WARMUP:-5}"
RUNS_PER_DATAPOINT="${RUNS_OVERRIDE:-10}"

RESULTS_DIR="${ROOT_DIR}/results-v5"
mkdir -p "$RESULTS_DIR"

BROKER_TLS="--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key"
BROKER_QUIC="--quic-host 0.0.0.0:14567"
CA="--ca-cert /opt/mqtt-certs/ca.pem"

declare -A ARM_URL ARM_FLAGS ARM_DELIVERY ARM_PUBCONN
ARM_URL[tcp-1conn]="mqtt://${BROKER_IP}:1883"
ARM_FLAGS[tcp-1conn]=""
ARM_DELIVERY[tcp-1conn]=""
ARM_PUBCONN[tcp-1conn]=1

ARM_URL[tcp-Nconn]="mqtt://${BROKER_IP}:1883"
ARM_FLAGS[tcp-Nconn]=""
ARM_DELIVERY[tcp-Nconn]=""
ARM_PUBCONN[tcp-Nconn]="$NTOPICS"

ARM_URL[quic-control]="quic://${BROKER_IP}:14567"
ARM_FLAGS[quic-control]="--quic-stream-strategy control-only ${CA}"
ARM_DELIVERY[quic-control]="--quic-delivery-strategy control-only"
ARM_PUBCONN[quic-control]=1

ARM_URL[quic-pertopic]="quic://${BROKER_IP}:14567"
ARM_FLAGS[quic-pertopic]="--quic-stream-strategy per-topic ${CA}"
ARM_DELIVERY[quic-pertopic]="--quic-delivery-strategy per-topic"
ARM_PUBCONN[quic-pertopic]=1

: "${E1_ARMS:=tcp-1conn tcp-Nconn quic-control quic-pertopic}"
read -ra ARMS <<< "$E1_ARMS"

apply_bottleneck() {
    local delay_ms="$1"
    local loss_pct="$2"
    CUR_NETEM_DELAY="$delay_ms"
    CUR_NETEM_LOSS="$loss_pct"
    ssh_broker "sudo bash /opt/mqtt-lib/experiments/netem/apply_bottleneck.sh ${delay_ms} ${RATE_MBIT} ${QUEUE_PKTS} ${loss_pct}"
}

restore_netem() {
    if [ -n "$CUR_NETEM_DELAY" ]; then
        if ! ssh_broker "sudo bash /opt/mqtt-lib/experiments/netem/apply_bottleneck.sh ${CUR_NETEM_DELAY} ${RATE_MBIT} ${QUEUE_PKTS} ${CUR_NETEM_LOSS}" 2>/dev/null; then
            echo "WARN: failed to restore bottleneck (rate=${RATE_MBIT}mbit delay=${CUR_NETEM_DELAY}ms loss=${CUR_NETEM_LOSS}%)" >&2
            return 1
        fi
    fi
    return 0
}

start_competing_flow() {
    local duration="$1"
    ssh_pub "pkill -x iperf3 2>/dev/null; sleep 1; iperf3 -s -D" >/dev/null 2>&1 || true
    sleep 1
    ssh_broker "pkill -x iperf3 2>/dev/null; nohup iperf3 -c ${PUB_INTERNAL_IP} -t ${duration} -J > /tmp/iperf_client.json 2>&1 & echo client-started" >/dev/null 2>&1 || true
}

wait_competing_flow() {
    ssh_broker "for _ in \$(seq 1 30); do pgrep -x iperf3 >/dev/null 2>&1 || break; sleep 1; done" 2>/dev/null || true
}

collect_competing_flow() {
    local output_dir="$1"
    local run_label="$2"
    scp -i "$SSH_KEY_PATH" $SSH_OPTS "${SSH_USER}@${BROKER_SSH_IP}:/tmp/iperf_client.json" \
        "${output_dir}/${run_label}_iperf.json" 2>/dev/null || true
}

collect_traces() {
    local output_dir="$1"
    local run_label="$2"
    for csv in messages.csv quinn_stats.csv; do
        scp -i "$SSH_KEY_PATH" $SSH_OPTS "${SSH_USER}@${PUB_IP}:/tmp/e1-traces/${csv}" \
            "${output_dir}/${run_label}_${csv}" 2>/dev/null || true
    done
    ssh_pub "rm -rf /tmp/e1-traces" 2>/dev/null || true
}

run_arm() {
    local label="$1"
    local bench_args="$2"
    local output_dir="${RESULTS_DIR}/${EXPERIMENT}"
    mkdir -p "$output_dir"
    ssh_pub "ulimit -n 65536; mqttv5 bench ${bench_args}" \
        > "${output_dir}/${label}.json" 2>/dev/null || true
    warn_if_empty "${output_dir}/${label}.json"
    echo "  saved: ${output_dir}/${label}.json"
}

for arm in "${ARMS[@]}"; do
    url="${ARM_URL[$arm]}"
    flags="${ARM_FLAGS[$arm]}"
    delivery="${ARM_DELIVERY[$arm]}"
    pubconn="${ARM_PUBCONN[$arm]}"

    stop_broker 2>/dev/null || true
    if ! start_broker "${BROKER_TLS} ${BROKER_QUIC} ${delivery}"; then
        echo "WARN: broker start failed for ${arm}, skipping arm" >&2
        continue
    fi

    for loss in "${LOSSES[@]}"; do
        apply_bottleneck "$DELAY" "$loss"
        label="${arm}_rate${RATE_MBIT}mbit_loss${loss}pct"
        echo "[${EXPERIMENT}] ${label}"

        bench_args="--url ${url} ${flags} --mode hol-blocking --topics ${NTOPICS} \
            --pub-connections ${pubconn} --sub-connections ${pubconn} \
            --duration ${DURATION} --warmup ${WARMUP} --payload-size 256 --qos 0 --rate ${OFFERED_RATE} \
            --trace-dir /tmp/e1-traces"

        for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
            run_label="${label}_run${run}"
            if [ -f "${RESULTS_DIR}/${EXPERIMENT}/${run_label}_messages.csv" ]; then
                echo "  skip (already complete): ${run_label}"
                continue
            fi
            if [ "$BROKER_FRESH" = "1" ]; then
                BROKER_FRESH=0
            elif ! restart_broker; then
                echo "WARN: broker restart failed, skipping ${run_label}" >&2
                continue
            fi
            start_monitors
            start_competing_flow $((DURATION + WARMUP + 5))
            run_arm "$run_label" "$bench_args"
            wait_competing_flow
            stop_monitors "${RESULTS_DIR}/${EXPERIMENT}" "$run_label"
            collect_competing_flow "${RESULTS_DIR}/${EXPERIMENT}" "$run_label"
            collect_traces "${RESULTS_DIR}/${EXPERIMENT}" "$run_label"
            sleep 5
        done

        clear_netem
    done
done

stop_broker
echo "experiment ${EXPERIMENT} v5 complete (group ${GROUP})"
