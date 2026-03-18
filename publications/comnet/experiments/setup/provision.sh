#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TF_DIR="${SCRIPT_DIR}/../terraform"

source "${SCRIPT_DIR}/config.env"

provision() {
    terraform -chdir="$TF_DIR" init -input=false

    terraform -chdir="$TF_DIR" apply -auto-approve \
        -var "project_id=${GCP_PROJECT_ID}"

    BROKER_IP=$(terraform -chdir="$TF_DIR" output -raw broker_ip)
    CLIENT_IP=$(terraform -chdir="$TF_DIR" output -raw client_ip)

    echo "BROKER_IP=${BROKER_IP}" >> "${SCRIPT_DIR}/config.env"
    echo "CLIENT_IP=${CLIENT_IP}" >> "${SCRIPT_DIR}/config.env"

    echo "broker: ${BROKER_IP}"
    echo "client: ${CLIENT_IP}"
    echo "IPs written to config.env"
}

teardown() {
    terraform -chdir="$TF_DIR" destroy -auto-approve \
        -var "project_id=${GCP_PROJECT_ID}"
    echo "done"
}

case "${1:-provision}" in
    provision) provision ;;
    teardown)  teardown ;;
    *)         echo "usage: $0 [provision|teardown]"; exit 1 ;;
esac
