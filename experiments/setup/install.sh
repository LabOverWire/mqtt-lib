#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/config.env"

: "${REPO_URL:?Set REPO_URL in config.env}"
: "${REPO_BRANCH:=main}"

install_on_host() {
    local host="$1"
    echo "installing on ${host}..."

    ssh -o StrictHostKeyChecking=no "root@${host}" bash -s <<'REMOTE'
        set -euo pipefail
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq build-essential pkg-config libssl-dev iproute2 openssl git
        if ! command -v rustup &>/dev/null; then
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        fi
        source "$HOME/.cargo/env"
        rustup update stable
REMOTE

    ssh "root@${host}" bash -s -- "$REPO_URL" "$REPO_BRANCH" <<'REMOTE'
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
            git clone --branch "$REPO_BRANCH" "$REPO_URL" /opt/mqtt-lib
            cd /opt/mqtt-lib
        fi
        cargo build --release -p mqttv5-cli
        ln -sf /opt/mqtt-lib/target/release/mqttv5 /usr/local/bin/mqttv5
        echo "mqttv5 installed: $(mqttv5 --version)"
REMOTE

    echo "done: ${host}"
}

install_on_host "${BROKER_IP}"
install_on_host "${CLIENT_IP}"
