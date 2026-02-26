#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="08_fanout"
SUBSCRIBER_COUNTS=(1 10 50 100)
LOSSES=(0 2 5)
DELAY=25

declare -A TRANSPORT_URLS
TRANSPORT_URLS[tcp]="mqtt://${BROKER_INTERNAL_IP}:1883"
TRANSPORT_URLS[quic-control]="quic://${BROKER_INTERNAL_IP}:14567"
TRANSPORT_URLS[quic-persub]="quic://${BROKER_INTERNAL_IP}:14567"

declare -A TRANSPORT_FLAGS
TRANSPORT_FLAGS[tcp]=""
TRANSPORT_FLAGS[quic-control]="--quic-stream-strategy control-only --ca-cert /certs/ca.pem"
TRANSPORT_FLAGS[quic-persub]="--quic-stream-strategy per-subscription --ca-cert /certs/ca.pem"

ensure_ca_secret
start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"

for tname in tcp quic-control quic-persub; do
    url="${TRANSPORT_URLS[$tname]}"
    flags="${TRANSPORT_FLAGS[$tname]}"

    for nsubs in "${SUBSCRIBER_COUNTS[@]}"; do
        for loss in "${LOSSES[@]}"; do
            apply_netem_broker "$DELAY" "$loss"
            label="${tname}_${nsubs}subs_loss${loss}pct"
            echo "[${EXPERIMENT}] ${label}"
            run_k8s_monitored "$EXPERIMENT" "$label" \
                "bench --url ${url} ${flags} --mode throughput --qos 0 --duration 30 --warmup 5 --payload-size 256 --publishers 1 --subscribers ${nsubs}"
            clear_netem_broker
        done
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete"
