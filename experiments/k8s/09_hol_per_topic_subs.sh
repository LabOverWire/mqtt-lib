#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="09_hol_per_topic_subs"
LOSSES=(0 1 2 5)
DELAY=25
TOPICS=64

declare -A TRANSPORT_URLS
TRANSPORT_URLS[tcp]="mqtt://${BROKER_INTERNAL_IP}:1883"
TRANSPORT_URLS[quic-control]="quic://${BROKER_INTERNAL_IP}:14567"
TRANSPORT_URLS[quic-pertopic]="quic://${BROKER_INTERNAL_IP}:14567"
TRANSPORT_URLS[quic-persub]="quic://${BROKER_INTERNAL_IP}:14567"
TRANSPORT_URLS[quic-perpub]="quic://${BROKER_INTERNAL_IP}:14567"

declare -A TRANSPORT_FLAGS
TRANSPORT_FLAGS[tcp]=""
TRANSPORT_FLAGS[quic-control]="--quic-stream-strategy control-only --ca-cert /certs/ca.pem"
TRANSPORT_FLAGS[quic-pertopic]="--quic-stream-strategy per-topic --ca-cert /certs/ca.pem"
TRANSPORT_FLAGS[quic-persub]="--quic-stream-strategy per-subscription --ca-cert /certs/ca.pem"
TRANSPORT_FLAGS[quic-perpub]="--quic-stream-strategy per-publish --ca-cert /certs/ca.pem"

ensure_ca_secret
start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"

for tname in tcp quic-control quic-pertopic quic-persub quic-perpub; do
    url="${TRANSPORT_URLS[$tname]}"
    flags="${TRANSPORT_FLAGS[$tname]}"

    for loss in "${LOSSES[@]}"; do
        apply_netem_broker "$DELAY" "$loss"
        label="${tname}_loss${loss}pct"
        echo "[${EXPERIMENT}] ${label}"
        run_k8s_monitored "$EXPERIMENT" "$label" \
            "bench --url ${url} ${flags} --mode hol-blocking --topics ${TOPICS} --duration 30 --warmup 5 --payload-size 512 --subscribers ${TOPICS}"
        clear_netem_broker
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete"
