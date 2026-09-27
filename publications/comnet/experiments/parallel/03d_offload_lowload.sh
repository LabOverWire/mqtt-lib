#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

EXPERIMENT="03d_offload_lowload_g${GROUP}"
LOSSES=(${LOSSES_OVERRIDE:-0 1 2 5 10})
DELAY=10
RUNS_PER_DATAPOINT="${RUNS_OVERRIDE:-5}"

RESULTS_DIR="${ROOT_DIR}/results-v5"
OUTPUT_DIR="${RESULTS_DIR}/${EXPERIMENT}"
mkdir -p "$OUTPUT_DIR"

DEV_CMD='DEV=$(ip route show default | awk "{print \$5}" | head -1)'

set_offload() {
    local mode="$1"
    if [ "$mode" = "off" ]; then
        ssh_broker "${DEV_CMD}; for f in gso tso gro lro tx-udp-segmentation rx-gro-list; do sudo ethtool -K \$DEV \$f off 2>/dev/null || true; done; echo '--- ethtool -k after off ---'; ethtool -k \$DEV | grep -E 'tcp-segmentation|generic-segmentation|generic-receive|large-receive|udp-segmentation'"
    else
        ssh_broker "${DEV_CMD}; for f in gso tso gro; do sudo ethtool -K \$DEV \$f on 2>/dev/null || true; done; echo '--- ethtool -k after on ---'; ethtool -k \$DEV | grep -E 'tcp-segmentation|generic-segmentation|generic-receive|large-receive|udp-segmentation'"
    fi
}

qdisc_snapshot() { ssh_broker "${DEV_CMD}; tc -s qdisc show dev \$DEV"; }

run_ablation_cell() {
    local label="$1"
    shift
    local bench_args="$*"
    for run in $(seq 1 "$RUNS_PER_DATAPOINT"); do
        local rl="${label}_run${run}"
        local f="${OUTPUT_DIR}/${rl}.json"
        if [ -s "$f" ] && [ "$(wc -c < "$f")" -gt 300 ]; then
            continue
        fi
        if [ "$BROKER_FRESH" = "1" ]; then
            BROKER_FRESH=0
        elif ! restart_broker; then
            echo "WARN: broker restart failed, skipping ${rl}" >&2
            continue
        fi
        start_monitors
        qdisc_snapshot > "${OUTPUT_DIR}/${rl}_qdisc_before.txt" 2>/dev/null || true
        run_bench_split "$EXPERIMENT" "$rl" "$bench_args"
        qdisc_snapshot > "${OUTPUT_DIR}/${rl}_qdisc_after.txt" 2>/dev/null || true
        stop_monitors "$OUTPUT_DIR" "$rl"
        sleep 5
    done
}

BENCH_COMMON="--mode throughput --duration 60 --warmup 5 --payload-size 256 --qos 0 --publishers 1 --subscribers 1 --inflight 64"

for offload in ${OFFLOAD_OVERRIDE:-on off}; do
    broker_flags="--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"
    if [ "$offload" = "off" ]; then
        broker_flags="${broker_flags} --quic-disable-offload"
    fi
    stop_broker 2>/dev/null || true
    echo "[${EXPERIMENT}] setting NIC offload ${offload} on broker"
    set_offload "$offload" | tee "${OUTPUT_DIR}/ethtool_${offload}.txt"
    if ! start_broker "$broker_flags"; then
        echo "ERROR: broker failed to start (offload ${offload})" >&2
        continue
    fi
    for loss in "${LOSSES[@]}"; do
        apply_netem "$DELAY" "$loss"
        echo "[${EXPERIMENT}] tls offload=${offload} loss=${loss}%"
        run_ablation_cell "tls_offload-${offload}_loss${loss}pct" \
            "--url mqtts://${BROKER_IP}:8883 --ca-cert /opt/mqtt-certs/ca.pem ${BENCH_COMMON}"
        echo "[${EXPERIMENT}] quic-control offload=${offload} loss=${loss}%"
        run_ablation_cell "quic-control_offload-${offload}_loss${loss}pct" \
            "--url quic://${BROKER_IP}:14567 --ca-cert /opt/mqtt-certs/ca.pem --quic-stream-strategy control-only ${BENCH_COMMON}"
        clear_netem
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete (group ${GROUP})"
