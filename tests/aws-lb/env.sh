#!/bin/bash
INFRA_DIR="/Volumes/SanDisk 4TB/repos/laboverwire/infrastructure/mqtt-lb-tests"

export LB_IP=$(cd "$INFRA_DIR" && tofu output -raw lb_ip)
export BACKEND_A_IP=$(cd "$INFRA_DIR" && tofu output -raw backend_a_ip)
export BACKEND_B_IP=$(cd "$INFRA_DIR" && tofu output -raw backend_b_ip)
export SSH_KEY="$HOME/.ssh/id_ed25519"
export SSH_USER="mqtt"
export CERT_DIR="/tmp/mqtt-lb-certs"
export REMOTE_CERT_DIR="/opt/mqtt-certs"

ssh_cmd() {
    local ip=$1
    shift
    ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no -o ConnectTimeout=10 "${SSH_USER}@${ip}" "$@"
}

scp_to() {
    local ip=$1
    local src=$2
    local dst=$3
    scp -i "$SSH_KEY" -o StrictHostKeyChecking=no "$src" "${SSH_USER}@${ip}:${dst}"
}
