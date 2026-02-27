#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXPERIMENT="02_hol_blocking"
LOSSES=(0 1 2 5)
DELAY=25

declare -A TRANSPORT_URLS
TRANSPORT_URLS[tcp]="mqtt://${BROKER_IP}:1883"
TRANSPORT_URLS[quic-control]="quic://${BROKER_IP}:14567"
TRANSPORT_URLS[quic-pertopic]="quic://${BROKER_IP}:14567"
TRANSPORT_URLS[quic-persub]="quic://${BROKER_IP}:14567"
TRANSPORT_URLS[quic-perpub]="quic://${BROKER_IP}:14567"

declare -A TRANSPORT_FLAGS
TRANSPORT_FLAGS[tcp]=""
TRANSPORT_FLAGS[quic-control]="--quic-stream-strategy control-only --ca-cert /opt/mqtt-certs/ca.pem"
TRANSPORT_FLAGS[quic-pertopic]="--quic-stream-strategy per-topic --ca-cert /opt/mqtt-certs/ca.pem"
TRANSPORT_FLAGS[quic-persub]="--quic-stream-strategy per-subscription --ca-cert /opt/mqtt-certs/ca.pem"
TRANSPORT_FLAGS[quic-perpub]="--quic-stream-strategy per-publish --ca-cert /opt/mqtt-certs/ca.pem"

start_broker "--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"

for tname in tcp quic-control quic-pertopic quic-persub quic-perpub; do
    url="${TRANSPORT_URLS[$tname]}"
    flags="${TRANSPORT_FLAGS[$tname]}"

    for loss in "${LOSSES[@]}"; do
        apply_netem "$DELAY" "$loss"
        label="${tname}_loss${loss}pct"
        echo "[${EXPERIMENT}] ${label}"
        run_monitored "$EXPERIMENT" "$label" \
            "--url ${url} ${flags} --mode hol-blocking --topics 8 --duration 30 --warmup 5 --payload-size 512"
        clear_netem
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete"
