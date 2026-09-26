#!/usr/bin/env bash
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${DIR}/group1.env"
KEY="$HOME/.ssh/id_ed25519"
OPTS="-i $KEY -o StrictHostKeyChecking=no -o BatchMode=yes -o ConnectTimeout=10"

cleanup() {
    ssh $OPTS "bench@${BROKER_SSH_IP}" "pkill -f '[m]qttv5 broker'; sudo tc qdisc del dev ens4 root" 2>/dev/null || true
}

run_resilient() {
    local label="$1"
    shift
    for attempt in $(seq 1 40); do
        echo "===== ${label} attempt ${attempt} ($(date '+%H:%M:%S')) ====="
        cleanup
        sleep 3
        if "$@"; then
            echo "===== ${label} COMPLETE on attempt ${attempt} ====="
            cleanup
            return 0
        fi
        echo "----- ${label} attempt ${attempt} aborted (transient), retrying -----"
        sleep 15
    done
    echo "===== ${label} GAVE UP after 40 attempts ====="
    cleanup
    return 1
}

run_resilient "exp3-qos1" env QOS_OVERRIDE=1 GROUP=1 bash "${DIR}/03_throughput_under_loss_v5.sh" || exit 1
run_resilient "exp3-qos1-tls" env QOS_OVERRIDE=1 GROUP=1 bash "${DIR}/03b_tls_throughput_v5.sh" || exit 1
run_resilient "exp4" env GROUP=1 bash "${DIR}/04_stream_strategies_v5.sh" || exit 1
echo "===== ALL RE-RUNS COMPLETE ($(date '+%H:%M:%S')) ====="
