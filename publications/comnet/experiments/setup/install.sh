#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/config.env"

: "${REPO_URL:?Set REPO_URL in config.env}"
: "${REPO_BRANCH:=main}"
: "${SSH_KEY_PATH:=$HOME/.ssh/id_ed25519}"

SSH_USER="${SSH_USER:-bench}"

install_on_host() {
    local host="$1"
    echo "installing on ${host}..."

    ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=accept-new "${SSH_USER}@${host}" bash -s <<'REMOTE'
        set -euo pipefail
        export DEBIAN_FRONTEND=noninteractive
        sudo apt-get update -qq
        sudo apt-get install -y -qq build-essential pkg-config libssl-dev iproute2 openssl git
        if ! command -v rustup &>/dev/null; then
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        fi
        source "$HOME/.cargo/env"
        rustup update stable
REMOTE

    ssh -i "$SSH_KEY_PATH" "${SSH_USER}@${host}" bash -s -- "$REPO_URL" "$REPO_BRANCH" <<'REMOTE'
        set -euo pipefail
        source "$HOME/.cargo/env"
        REPO_URL="$1"
        REPO_BRANCH="$2"
        if [ -d /opt/mqtt-lib ]; then
            cd /opt/mqtt-lib
            git fetch origin
            git checkout "$REPO_BRANCH"
            git pull
        else
            sudo git clone --branch "$REPO_BRANCH" "$REPO_URL" /opt/mqtt-lib
            sudo chown -R "$(whoami):$(whoami)" /opt/mqtt-lib
            cd /opt/mqtt-lib
        fi
        cargo build --release -p mqttv5-cli
        sudo ln -sf /opt/mqtt-lib/target/release/mqttv5 /usr/local/bin/mqttv5
        echo "mqttv5 installed: $(mqttv5 --version)"
REMOTE

    echo "done: ${host}"
}

install_on_host "${BROKER_IP}"
install_on_host "${CLIENT_IP}"
