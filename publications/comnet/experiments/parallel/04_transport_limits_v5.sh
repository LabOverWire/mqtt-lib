#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

EXPERIMENT="04_transport_limits"
: "${PART:?Set PART=A|B|C}"
RUNS_PER_DATAPOINT="${RUNS_PER_DATAPOINT:-10}"
COLLECT_QUIC_STATS=1

RESULTS_DIR="${ROOT_DIR}/results-v5"
OUTPUT_DIR="${RESULTS_DIR}/${EXPERIMENT}"
mkdir -p "$OUTPUT_DIR"

CA="--ca-cert /opt/mqtt-certs/ca.pem"
BROKER_BASE="--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"

case "$PART" in
    A)
        CELLS=()
        for strategy in control-only per-topic per-publish; do
            for topics in 1 2 4 8 16; do
                CELLS+=("${strategy} ${topics} def def 25 2 tput 0")
            done
        done
        ;;
    B)
        CELLS=(
            "per-publish 8 25 def 25 2 tput 0"
            "per-publish 8 100 def 25 2 tput 0"
            "per-publish 8 250 def 25 2 tput 0"
            "per-publish 8 1000 def 25 2 tput 0"
            "control-only 8 100 def 25 2 tput 0"
            "control-only 8 1000 def 25 2 tput 0"
            "per-topic 8 100 def 25 2 tput 0"
            "per-topic 8 1000 def 25 2 tput 0"
            "per-publish 8 100 def 10 2 tput 0"
            "per-publish 8 100 def 50 2 tput 0"
            "control-only 8 100 def 10 2 tput 0"
            "control-only 8 100 def 50 2 tput 0"
            "per-publish 8 100 def 25 5 hol 2000"
            "per-publish 8 1000 def 25 5 hol 2000"
        )
        ;;
    C)
        CELLS=()
        for window in 131072 262144 1048576; do
            CELLS+=("control-only 8 def ${window} 25 2 tput 0")
            CELLS+=("per-topic 1 def ${window} 25 2 tput 0")
            CELLS+=("per-topic 8 def ${window} 25 2 tput 0")
        done
        CELLS+=("control-only 8 def 262144 25 0 tput 0")
        CELLS+=("per-topic 8 def 262144 25 0 tput 0")
        CELLS+=("control-only 8 def 1048576 25 0 tput 0")
        ;;
    *)
        echo "unknown PART ${PART}" >&2
        exit 1
        ;;
esac

collect_broker_quic_stats() {
    local run_label="$1"
    local index=0
    local remote
    for remote in $(ssh_broker "ls /tmp/quic-stats/*.csv 2>/dev/null" || true); do
        index=$((index + 1))
        scp -q -i "$SSH_KEY_PATH" "${SSH_USER}@${BROKER_SSH_IP}:${remote}" \
            "${OUTPUT_DIR}/${run_label}_broker_quic_${index}.csv" || true
    done
}

run_hol_colocated() {
    local run_label="$1"
    shift
    ssh_pub "ulimit -n 65536; mqttv5 bench $*" > "${OUTPUT_DIR}/${run_label}.json" 2>/dev/null || true
    warn_if_empty "${OUTPUT_DIR}/${run_label}.json"
}

for cell in "${CELLS[@]}"; do
    read -r strategy topics streams window delay loss mode rate <<< "$cell"
    label="${strategy}_t${topics}_s${streams}_w${window}_d${delay}_l${loss}_${mode}"
    if [ "$mode" = "hol" ]; then
        label="${label}_r${rate}"
    fi

    broker_flags="${BROKER_BASE} --quic-delivery-strategy ${strategy}"
    client_flags=""
    if [ "$streams" != "def" ]; then
        broker_flags="${broker_flags} --quic-max-streams ${streams}"
        client_flags="--quic-max-streams ${streams}"
    fi
    if [ "$window" != "def" ]; then
        broker_flags="${broker_flags} --quic-stream-window ${window}"
    fi

    if [ "$mode" = "hol" ]; then
        bench_args="--url quic://${BROKER_IP}:14567 ${CA} --quic-stream-strategy ${strategy} ${client_flags} --mode hol-blocking --topics ${topics} --duration 60 --warmup 5 --payload-size 256 --rate ${rate}"
    else
        bench_args="--url quic://${BROKER_IP}:14567 ${CA} --quic-stream-strategy ${strategy} ${client_flags} --mode throughput --duration 60 --warmup 5 --payload-size 256 --publishers 1 --topics ${topics} --subscribers 1 --inflight 64"
    fi

    pending=0
    for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
        if [ ! -s "${OUTPUT_DIR}/${label}_run${run}.json" ]; then
            pending=$((pending + 1))
        fi
    done
    if [ "$pending" = "0" ]; then
        echo "[${EXPERIMENT}:${PART}] ${label} complete, skipping"
        continue
    fi

    clear_netem 2>/dev/null || true
    stop_broker 2>/dev/null || true
    if ! start_broker "$broker_flags"; then
        echo "WARN: broker start failed for ${label}, skipping" >&2
        continue
    fi
    apply_netem "$delay" "$loss"
    echo "[${EXPERIMENT}:${PART}] ${label} (${pending} runs pending)"

    for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
        run_label="${label}_run${run}"
        if [ -s "${OUTPUT_DIR}/${run_label}.json" ]; then
            continue
        fi
        if [ "$BROKER_FRESH" = "1" ]; then
            BROKER_FRESH=0
        elif ! restart_broker; then
            echo "WARN: broker restart failed, skipping ${run_label}" >&2
            continue
        fi
        start_monitors
        if [ "$mode" = "hol" ]; then
            run_hol_colocated "$run_label" "$bench_args"
        else
            run_bench_split "$EXPERIMENT" "$run_label" "$bench_args"
        fi
        stop_monitors "$OUTPUT_DIR" "$run_label"
        collect_broker_quic_stats "$run_label"
        sleep 5
    done
    clear_netem
done

stop_broker
echo "experiment ${EXPERIMENT} part ${PART} complete (group ${GROUP})"
