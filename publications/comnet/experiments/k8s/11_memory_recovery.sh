#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="11_memory_recovery"
CYCLES=10
LOAD_DURATION=60
IDLE_DURATION=30

ensure_ca_secret

for tname in tcp quic-pertopic; do
    if [ "$tname" = "tcp" ]; then
        broker_flags="--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key"
        bench_url="mqtt://${BROKER_INTERNAL_IP}:1883"
        bench_flags=""
    else
        broker_flags="--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"
        bench_url="quic://${BROKER_INTERNAL_IP}:14567"
        bench_flags="--quic-stream-strategy per-topic --ca-cert /certs/ca.pem"
    fi

    start_broker "$broker_flags"
    apply_netem_broker 25 5

    output_dir="${RESULTS_DIR}/${EXPERIMENT}"
    mkdir -p "$output_dir"

    start_monitor "${output_dir}/${tname}_broker_resources.csv"

    for cycle in $(seq 1 "$CYCLES"); do
        echo "[${EXPERIMENT}] ${tname} cycle ${cycle}/${CYCLES} - LOAD"
        run_k8s_bench "$EXPERIMENT" "${tname}_cycle${cycle}" \
            "bench --url ${bench_url} ${bench_flags} --mode throughput --qos 1 --duration ${LOAD_DURATION} --warmup 0 --publishers 64 --subscribers 1"

        echo "[${EXPERIMENT}] ${tname} cycle ${cycle}/${CYCLES} - IDLE (${IDLE_DURATION}s)"
        sleep "$IDLE_DURATION"
    done

    stop_monitor "${output_dir}/${tname}_broker_resources.csv"
    clear_netem_broker
    stop_broker
done

echo "experiment ${EXPERIMENT} complete"
