#!/usr/bin/env bash
set -euo pipefail
export GROUP="${GROUP:-1}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bash "${SCRIPT_DIR}/01_subscriber_flow_delivery.sh"
