#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

: "${SUB_IP:?Set SUB_IP in config.env}"
ssh_sub() { ssh -i "$SSH_KEY_PATH" $SSH_OPTS "${SSH_USER}@${SUB_IP}" "$@"; }

EXPERIMENT="14_payload_format_3host"
FORMATS=(raw json bebytes compressed-json)
SIZES=(64 256 1024 4096)
DURATION=30
WARMUP=5
QOS=0
PUBLISHERS=4

start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key"

for fmt in "${FORMATS[@]}"; do
    for size in "${SIZES[@]}"; do
        label="${fmt}_${size}b_throughput"
        echo "[${EXPERIMENT}] ${label}"

        output_dir="${RESULTS_DIR}/${EXPERIMENT}"
        mkdir -p "$output_dir"

        for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
            run_label="${label}_run${run}"
            start_monitor "${output_dir}/${run_label}_broker_resources.csv"

            echo "  starting subscriber on sub VM..."
            SUB_PID=$(ssh_sub "ulimit -n 65536; nohup mqttv5 bench \
                --url mqtts://${BROKER_IP}:8883 --ca-cert /opt/mqtt-certs/ca.pem \
                --mode throughput --duration $((DURATION + WARMUP + 5)) --warmup 0 \
                --payload-size ${size} --payload-format ${fmt} --qos ${QOS} \
                --publishers 0 --subscribers 1 \
                > /tmp/sub_bench.json 2>/dev/null & echo \$!")
            sleep 2

            echo "  starting publisher on client VM..."
            ssh_client "ulimit -n 65536; mqttv5 bench \
                --url mqtts://${BROKER_IP}:8883 --ca-cert /opt/mqtt-certs/ca.pem \
                --mode throughput --duration ${DURATION} --warmup ${WARMUP} \
                --payload-size ${size} --payload-format ${fmt} --qos ${QOS} \
                --publishers ${PUBLISHERS} --subscribers 0" \
                > "${output_dir}/${run_label}_pub.json" 2>/dev/null

            sleep 3
            ssh_sub "kill ${SUB_PID}" 2>/dev/null || true
            scp -i "$SSH_KEY_PATH" "${SSH_USER}@${SUB_IP}:/tmp/sub_bench.json" \
                "${output_dir}/${run_label}_sub.json"

            stop_monitor "${output_dir}/${run_label}_broker_resources.csv"
            sleep 5
        done
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete"
