#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
source "${ROOT_DIR}/setup/config.env"

: "${BROKER_IP:?Set BROKER_IP in config.env}"
: "${SSH_KEY_PATH:=$HOME/.ssh/id_ed25519}"
: "${RUNS_PER_DATAPOINT:=5}"

SSH_USER="${SSH_USER:-bench}"
RESULTS_DIR="${ROOT_DIR}/results-phase1"
BROKER_INTERNAL_IP="10.138.0.5"
IMAGE="us-west1-docker.pkg.dev/quic-experiments-488321/mqoq-bench/mqttv5:bench"

KUBECTL="${KUBECTL:-$(command -v kubectl 2>/dev/null || echo /opt/homebrew/bin/kubectl)}"

mkdir -p "$RESULTS_DIR"

ssh_broker() {
    ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no "${SSH_USER}@${BROKER_IP}" "$@"
}

BROKER_PID=""

start_broker() {
    local extra_flags="${1:-}"
    echo "starting broker on ${BROKER_IP}..."
    BROKER_PID=$(ssh_broker "ulimit -n 65536; nohup mqttv5 broker --allow-anonymous --host 0.0.0.0:1883 --storage-backend memory --max-clients 50000 \
        ${extra_flags} > /tmp/broker.log 2>&1 & echo \$!")
    sleep 2
    echo "broker pid: ${BROKER_PID}"
}

stop_broker() {
    echo "stopping broker..."
    ssh_broker "pkill -f 'mqttv5 broker'" 2>/dev/null || true
    BROKER_PID=""
    sleep 1
}

apply_netem_broker() {
    local delay_ms="$1"
    local loss_pct="${2:-0}"
    ssh_broker "sudo bash /opt/mqtt-lib/experiments/netem/apply.sh ${delay_ms} ${loss_pct}"
}

clear_netem_broker() {
    ssh_broker "sudo bash /opt/mqtt-lib/experiments/netem/clear.sh"
}

MONITOR_PID=""

start_monitor() {
    MONITOR_PID=$(ssh_broker "nohup bash /opt/mqtt-lib/experiments/monitor/resource_monitor.sh ${BROKER_PID} \
        > /tmp/monitor.csv 2>&1 & echo \$!")
    echo "monitor pid: ${MONITOR_PID}"
}

stop_monitor() {
    local output_file="$1"
    ssh_broker "kill ${MONITOR_PID}" 2>/dev/null || true
    scp -i "$SSH_KEY_PATH" "${SSH_USER}@${BROKER_IP}:/tmp/monitor.csv" "$output_file"
    MONITOR_PID=""
}

generate_job_name() {
    local label="$1"
    local sanitized
    sanitized=$(echo "$label" | tr '[:upper:]_' '[:lower:]-' | sed 's/[^a-z0-9-]//g' | cut -c1-50)
    echo "bench-${sanitized}-$(date +%s)"
}

run_k8s_bench() {
    local experiment="$1"
    local label="$2"
    shift 2
    local bench_args="$*"

    local output_dir="${RESULTS_DIR}/${experiment}"
    mkdir -p "$output_dir"
    local output_file="${output_dir}/${label}.json"
    local job_name
    job_name=$(generate_job_name "$label")

    IFS=' ' read -ra args_array <<< "$bench_args"

    local args_json="["
    local first=true
    for arg in "${args_array[@]}"; do
        if [ "$first" = true ]; then
            first=false
        else
            args_json+=","
        fi
        args_json+="\"${arg}\""
    done
    args_json+="]"

    cat <<EOF | $KUBECTL apply -f -
apiVersion: batch/v1
kind: Job
metadata:
  name: ${job_name}
  labels:
    app: mqoq-bench
    experiment: ${experiment}
    label: ${label}
spec:
  backoffLimit: 0
  ttlSecondsAfterFinished: 300
  template:
    metadata:
      labels:
        app: mqoq-bench
        experiment: ${experiment}
    spec:
      restartPolicy: Never
      containers:
        - name: bench
          image: ${IMAGE}
          args: ${args_json}
          resources:
            requests:
              cpu: "2"
              memory: 2Gi
            limits:
              cpu: "4"
              memory: 4Gi
          volumeMounts:
            - name: ca-cert
              mountPath: /certs
              readOnly: true
      volumes:
        - name: ca-cert
          secret:
            secretName: mqoq-ca-cert
EOF

    echo "  waiting for job ${job_name}..."
    $KUBECTL wait --for=condition=complete --timeout=300s "job/${job_name}" 2>/dev/null || {
        echo "  job ${job_name} failed or timed out"
        $KUBECTL logs "job/${job_name}" > "$output_file" 2>/dev/null || true
        $KUBECTL delete job "${job_name}" 2>/dev/null || true
        return 1
    }

    $KUBECTL logs "job/${job_name}" > "$output_file" 2>/dev/null
    $KUBECTL delete job "${job_name}" 2>/dev/null || true
    echo "  saved: ${output_file}"
}

run_k8s_monitored() {
    local experiment="$1"
    local label="$2"
    shift 2
    local bench_args="$*"
    local output_dir="${RESULTS_DIR}/${experiment}"
    mkdir -p "$output_dir"

    for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
        local run_label="${label}_run${run}"
        start_monitor "${output_dir}/${run_label}_broker_resources.csv"
        run_k8s_bench "$experiment" "$run_label" "$bench_args"
        stop_monitor "${output_dir}/${run_label}_broker_resources.csv"
    done
}

ensure_ca_secret() {
    if ! $KUBECTL get secret mqoq-ca-cert &>/dev/null; then
        echo "creating CA cert secret..."
        local tmpfile
        tmpfile=$(mktemp)
        scp -i "$SSH_KEY_PATH" "${SSH_USER}@${BROKER_IP}:/opt/mqtt-certs/ca.pem" "$tmpfile"
        $KUBECTL create secret generic mqoq-ca-cert --from-file=ca.pem="$tmpfile"
        rm -f "$tmpfile"
        echo "CA cert secret created"
    fi
}
