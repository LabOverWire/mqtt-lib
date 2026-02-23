#!/usr/bin/env bash
set -euo pipefail

BROKER_IP="${1:?usage: $0 <BROKER_IP>}"
CERT_DIR="/opt/mqtt-certs"
DAYS=365

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/config.env"

generate_certs() {
    ssh "root@${BROKER_IP}" bash -s -- "$BROKER_IP" "$CERT_DIR" "$DAYS" <<'REMOTE'
        set -euo pipefail
        BROKER_IP="$1"
        CERT_DIR="$2"
        DAYS="$3"

        mkdir -p "$CERT_DIR"
        cd "$CERT_DIR"

        openssl ecparam -name prime256v1 -genkey -noout -out ca.key
        openssl req -new -x509 -key ca.key -out ca.pem -days "$DAYS" \
            -subj "/CN=MQoQ Bench CA"

        openssl ecparam -name prime256v1 -genkey -noout -out server.key
        openssl req -new -key server.key -out server.csr \
            -subj "/CN=${BROKER_IP}"

        cat > server_ext.cnf <<EOF
[v3_req]
subjectAltName = IP:${BROKER_IP}
EOF

        openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key \
            -CAcreateserial -out server.pem -days "$DAYS" \
            -extfile server_ext.cnf -extensions v3_req

        rm -f server.csr server_ext.cnf ca.srl
        echo "certificates generated in ${CERT_DIR}"
        ls -la "$CERT_DIR"
REMOTE

    echo "copying CA cert to client..."
    ssh "root@${BROKER_IP}" "cat ${CERT_DIR}/ca.pem" | \
        ssh "root@${CLIENT_IP}" "mkdir -p ${CERT_DIR} && cat > ${CERT_DIR}/ca.pem"

    echo "done"
}

generate_certs
