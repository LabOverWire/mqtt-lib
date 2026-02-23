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

declare -A TRANSPORT_FLAGS
TRANSPORT_FLAGS[tcp]=""
TRANSPORT_FLAGS[quic-control]="--quic-stream-strategy control-only --insecure"
TRANSPORT_FLAGS[quic-pertopic]="--quic-stream-strategy per-topic --insecure"

start_broker "--quic-host 0.0.0.0:14567 --quic-cert /opt/mqtt-certs/server.pem --quic-key /opt/mqtt-certs/server.key"

for tname in tcp quic-control quic-pertopic; do
    url="${TRANSPORT_URLS[$tname]}"
    flags="${TRANSPORT_FLAGS[$tname]}"

    for loss in "${LOSSES[@]}"; do
        apply_netem "$DELAY" "$loss"
        label="${tname}_loss${loss}pct"
        echo "[${EXPERIMENT}] ${label}"
        run_repeated "$EXPERIMENT" "$label" \
            "--url ${url} ${flags} --mode hol-blocking --topics 4 --duration 30"
        clear_netem
    done
done

stop_broker
echo "experiment ${EXPERIMENT} complete"
