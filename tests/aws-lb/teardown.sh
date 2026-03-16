#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Stopping brokers ==="
for ip in "$LB_IP" "$BACKEND_A_IP" "$BACKEND_B_IP"; do
    ssh_cmd "$ip" "killall mqttv5 || true" 2>/dev/null
    echo "  Stopped $ip"
done

echo "=== Cleaning up local certs ==="
rm -rf "$CERT_DIR"

if [ "${1:-}" = "--destroy" ]; then
    echo "=== Destroying infrastructure ==="
    INFRA_DIR="/Volumes/SanDisk 4TB/repos/laboverwire/infrastructure/mqtt-lb-tests"
    cd "$INFRA_DIR"
    tofu destroy -auto-approve
    echo "Infrastructure destroyed"
else
    echo ""
    echo "VMs are still running. To destroy:"
    echo "  cd '/Volumes/SanDisk 4TB/repos/laboverwire/infrastructure/mqtt-lb-tests'"
    echo "  tofu destroy"
fi
