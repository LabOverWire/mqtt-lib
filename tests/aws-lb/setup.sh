#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Waiting for builds to complete ==="
for name_ip in "LB:$LB_IP" "Backend-A:$BACKEND_A_IP" "Backend-B:$BACKEND_B_IP"; do
    name="${name_ip%%:*}"
    ip="${name_ip##*:}"
    echo -n "Waiting for $name ($ip)..."
    for i in $(seq 1 60); do
        if ssh_cmd "$ip" "test -f /tmp/build-done" 2>/dev/null; then
            echo " ready"
            break
        fi
        if [ "$i" -eq 60 ]; then
            echo " TIMEOUT"
            echo "Build not complete on $name after 10 minutes"
            exit 1
        fi
        sleep 10
        echo -n "."
    done
done

echo "=== Verifying binaries ==="
for ip in "$LB_IP" "$BACKEND_A_IP" "$BACKEND_B_IP"; do
    version=$(ssh_cmd "$ip" "mqttv5 --version" 2>&1)
    echo "  $ip: $version"
done

echo "=== Generating TLS certificates ==="
rm -rf "$CERT_DIR"
mkdir -p "$CERT_DIR"

openssl genrsa -out "$CERT_DIR/ca.key" 2048
openssl req -new -x509 -days 30 -key "$CERT_DIR/ca.key" -out "$CERT_DIR/ca.pem" \
    -subj "/CN=MQTT LB Test CA" -sha256

for name_ip in "lb:$LB_IP" "backend-a:$BACKEND_A_IP" "backend-b:$BACKEND_B_IP"; do
    name="${name_ip%%:*}"
    ip="${name_ip##*:}"

    openssl ecparam -genkey -name prime256v1 | \
        openssl pkcs8 -topk8 -nocrypt -out "$CERT_DIR/${name}.key"

    openssl req -new -key "$CERT_DIR/${name}.key" -out "$CERT_DIR/${name}.csr" \
        -subj "/CN=mqtt-${name}"

    cat > "$CERT_DIR/${name}_ext.cnf" <<EOF
basicConstraints = CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = IP:${ip}
EOF

    openssl x509 -req -days 30 -in "$CERT_DIR/${name}.csr" \
        -CA "$CERT_DIR/ca.pem" -CAkey "$CERT_DIR/ca.key" \
        -CAcreateserial -out "$CERT_DIR/${name}.pem" \
        -sha256 -extfile "$CERT_DIR/${name}_ext.cnf"

    rm -f "$CERT_DIR/${name}.csr" "$CERT_DIR/${name}_ext.cnf"
done

echo "=== Distributing certificates ==="
for name_ip in "lb:$LB_IP" "backend-a:$BACKEND_A_IP" "backend-b:$BACKEND_B_IP"; do
    name="${name_ip%%:*}"
    ip="${name_ip##*:}"

    scp_to "$ip" "$CERT_DIR/ca.pem" "$REMOTE_CERT_DIR/ca.pem"
    scp_to "$ip" "$CERT_DIR/${name}.pem" "$REMOTE_CERT_DIR/server.pem"
    scp_to "$ip" "$CERT_DIR/${name}.key" "$REMOTE_CERT_DIR/server.key"
    echo "  Certs deployed to $name ($ip)"
done

echo "=== Generating LB broker config files ==="

BASE_CONFIG=$(ssh_cmd "$LB_IP" "mqttv5 broker generate-config --format json")

gen_lb_config() {
    local transport=$1
    local backends_json=$2
    local bind_json=$3
    local tls_json=$4
    local quic_json=$5

    echo "$BASE_CONFIG" | jq \
        --argjson bind "$bind_json" \
        --argjson tls "$tls_json" \
        --argjson quic "$quic_json" \
        --argjson backends "$backends_json" \
        '.bind_addresses = $bind
         | .tls_config = $tls
         | .quic_config = $quic
         | .websocket_config = null
         | .websocket_tls_config = null
         | .storage_config.backend = "Memory"
         | .storage_config.enable_persistence = false
         | .load_balancer = {"backends": $backends}'
}

TCP_BACKENDS="[\"mqtt://${BACKEND_A_IP}:1883\", \"mqtt://${BACKEND_B_IP}:1883\"]"
gen_lb_config "tcp" \
    "$TCP_BACKENDS" \
    '["0.0.0.0:1883"]' \
    'null' \
    'null' \
    > /tmp/lb-tcp.json

TLS_BACKENDS="[\"mqtts://${BACKEND_A_IP}:8883\", \"mqtts://${BACKEND_B_IP}:8883\"]"
TLS_CONFIG=$(jq -n \
    --arg cert "$REMOTE_CERT_DIR/server.pem" \
    --arg key "$REMOTE_CERT_DIR/server.key" \
    '{cert_file: $cert, key_file: $key, ca_file: null, require_client_cert: false, bind_addresses: ["0.0.0.0:8883"]}')
gen_lb_config "tls" \
    "$TLS_BACKENDS" \
    '["0.0.0.0:1883"]' \
    "$TLS_CONFIG" \
    'null' \
    > /tmp/lb-tls.json

QUIC_BACKENDS="[\"quic://${BACKEND_A_IP}:14567\", \"quic://${BACKEND_B_IP}:14567\"]"
QUIC_CONFIG=$(jq -n \
    --arg cert "$REMOTE_CERT_DIR/server.pem" \
    --arg key "$REMOTE_CERT_DIR/server.key" \
    '{cert_file: $cert, key_file: $key, ca_file: null, require_client_cert: false, bind_addresses: ["0.0.0.0:14567"]}')
gen_lb_config "quic" \
    "$QUIC_BACKENDS" \
    '["0.0.0.0:1883"]' \
    'null' \
    "$QUIC_CONFIG" \
    > /tmp/lb-quic.json

scp_to "$LB_IP" /tmp/lb-tcp.json "$REMOTE_CERT_DIR/lb-tcp.json"
scp_to "$LB_IP" /tmp/lb-tls.json "$REMOTE_CERT_DIR/lb-tls.json"
scp_to "$LB_IP" /tmp/lb-quic.json "$REMOTE_CERT_DIR/lb-quic.json"
echo "  LB configs deployed"

echo "=== Setup complete ==="
echo "LB:        $LB_IP"
echo "Backend A: $BACKEND_A_IP"
echo "Backend B: $BACKEND_B_IP"
echo "Local CA:  $CERT_DIR/ca.pem"
