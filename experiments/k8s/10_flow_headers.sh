#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="10_flow_headers"
LOSSES=(0 1 2 5)
DELAY=25
PUBLISHERS=64
SUBSCRIBERS=64

declare -A TRANSPORT_URLS
TRANSPORT_URLS[quic-pertopic]="quic://${BROKER_INTERNAL_IP}:14567"
TRANSPORT_URLS[quic-persub]="quic://${BROKER_INTERNAL_IP}:14567"

declare -A TRANSPORT_FLAGS
TRANSPORT_FLAGS[quic-pertopic]="--quic-stream-strategy per-topic --quic-flow-headers --ca-cert /certs/ca.pem"
TRANSPORT_FLAGS[quic-persub]="--quic-stream-strategy per-subscription --quic-flow-headers --ca-cert /certs/ca.pem"

ensure_ca_secret
start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"

for tname in quic-pertopic quic-persub; do
    url="${TRANSPORT_URLS[$tname]}"
    flags="${TRANSPORT_FLAGS[$tname]}"

    for qos in 0 1; do
        for loss in "${LOSSES[@]}"; do
            apply_netem_broker "$DELAY" "$loss"
            label="${tname}_qos${qos}_loss${loss}pct"
            echo "[${EXPERIMENT}] ${label}"
            run_k8s_monitored "$EXPERIMENT" "$label" \
                "bench --url ${url} ${flags} --mode throughput --qos ${qos} --duration 30 --warmup 5 --payload-size 256 --publishers ${PUBLISHERS} --subscribers ${SUBSCRIBERS}"
            clear_netem_broker
        done
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete"
