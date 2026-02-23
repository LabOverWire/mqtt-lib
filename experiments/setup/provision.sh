#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/config.env"

: "${DO_SSH_KEY_ID:?Set DO_SSH_KEY_ID in config.env}"
: "${DO_REGION:=nyc3}"
: "${DO_SIZE:=s-4vcpu-8gb}"
: "${DO_IMAGE:=ubuntu-24-04-x64}"

provision() {
    echo "creating mqoq-broker droplet..."
    doctl compute droplet create mqoq-broker \
        --region "$DO_REGION" \
        --size "$DO_SIZE" \
        --image "$DO_IMAGE" \
        --ssh-keys "$DO_SSH_KEY_ID" \
        --wait \
        --format ID,PublicIPv4 --no-header

    echo "creating mqoq-client droplet..."
    doctl compute droplet create mqoq-client \
        --region "$DO_REGION" \
        --size "$DO_SIZE" \
        --image "$DO_IMAGE" \
        --ssh-keys "$DO_SSH_KEY_ID" \
        --wait \
        --format ID,PublicIPv4 --no-header

    sleep 5

    BROKER_IP=$(doctl compute droplet list --format Name,PublicIPv4 --no-header | grep mqoq-broker | awk '{print $2}')
    CLIENT_IP=$(doctl compute droplet list --format Name,PublicIPv4 --no-header | grep mqoq-client | awk '{print $2}')

    echo "BROKER_IP=${BROKER_IP}" >> "${SCRIPT_DIR}/config.env"
    echo "CLIENT_IP=${CLIENT_IP}" >> "${SCRIPT_DIR}/config.env"

    echo "broker: ${BROKER_IP}"
    echo "client: ${CLIENT_IP}"
    echo "IPs written to config.env"
}

teardown() {
    echo "destroying mqoq droplets..."
    doctl compute droplet delete mqoq-broker --force 2>/dev/null || true
    doctl compute droplet delete mqoq-client --force 2>/dev/null || true
    echo "done"
}

case "${1:-provision}" in
    provision) provision ;;
    teardown)  teardown ;;
    *)         echo "usage: $0 [provision|teardown]"; exit 1 ;;
esac
