#!/usr/bin/env bash
source "$(dirname "${BASH_SOURCE[0]}")/group1.env"
KEY="$HOME/.ssh/id_ed25519"
OPTS="-i $KEY -o StrictHostKeyChecking=no -o BatchMode=yes -o ConnectTimeout=10"

cleanup() {
    ssh $OPTS "bench@${BROKER_SSH_IP}" "pkill -f '[m]qttv5 broker'; sudo tc qdisc del dev ens4 root" 2>/dev/null || true
}

for attempt in $(seq 1 40); do
    echo "===== TLS sweep attempt ${attempt} ($(date '+%H:%M:%S')) ====="
    cleanup
    sleep 3
    if GROUP=1 bash "$(dirname "${BASH_SOURCE[0]}")/03b_tls_throughput_v5.sh"; then
        echo "===== TLS sweep COMPLETE on attempt ${attempt} ====="
        cleanup
        exit 0
    fi
    echo "----- attempt ${attempt} aborted (transient), retrying -----"
    sleep 15
done
echo "===== TLS sweep gave up after 40 attempts ====="
cleanup
exit 1
