#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="06_resource_overhead"
STRATEGIES=("control-only" "per-publish" "per-topic" "per-subscription")
CONCURRENCIES=(10 50 100)
RUNS_PER_DATAPOINT=3

start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"

for conc in "${CONCURRENCIES[@]}"; do
    label="tcp_${conc}conn"
    echo "[${EXPERIMENT}] ${label}"
    run_monitored "$EXPERIMENT" "$label" \
        "--url mqtt://${BROKER_IP}:1883 --mode throughput --duration 60 --warmup 5 --payload-size 256 --publishers ${conc} --subscribers ${conc}"

    for strategy in "${STRATEGIES[@]}"; do
        label="quic-${strategy}_${conc}conn"
        echo "[${EXPERIMENT}] ${label}"
        run_monitored "$EXPERIMENT" "$label" \
            "--url quic://${BROKER_IP}:14567 --ca-cert /opt/mqtt-certs/ca.pem --quic-stream-strategy ${strategy} --mode throughput --duration 60 --warmup 5 --payload-size 256 --publishers ${conc} --subscribers ${conc}"
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete"
