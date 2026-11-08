#!/bin/bash

# Script to generate test certificates for MQTT TLS testing
# These certificates are for testing purposes only

set -e

# Configuration
CERT_DIR="test_certs"
DAYS_VALID=365
COUNTRY="US"
STATE="Test"
CITY="Test"
ORG="Test"
CN_CA="Test CA"
CN_CLIENT="Test Client"

# Create directory if it doesn't exist
mkdir -p "$CERT_DIR"

echo "Generating test certificates in $CERT_DIR..."

# Generate CA private key
openssl genrsa -out "$CERT_DIR/ca.key" 2048

# Generate CA certificate
openssl req -new -x509 -days "$DAYS_VALID" -key "$CERT_DIR/ca.key" -out "$CERT_DIR/ca.pem" \
    -subj "/C=$COUNTRY/ST=$STATE/L=$CITY/O=$ORG/CN=$CN_CA" \
    -sha256

# Generate client private key (ECDSA P-256 for DTLS compatibility) in PKCS#8 format
openssl ecparam -genkey -name prime256v1 | openssl pkcs8 -topk8 -nocrypt -out "$CERT_DIR/client.key"

# Generate client certificate request
openssl req -new -key "$CERT_DIR/client.key" -out "$CERT_DIR/client.csr" \
    -subj "/C=$COUNTRY/ST=$STATE/L=$CITY/O=$ORG/CN=$CN_CLIENT"

# Create extensions file for client certificate
cat > "$CERT_DIR/client_ext.cnf" <<EOF
basicConstraints = CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = clientAuth
EOF

# Sign client certificate with CA including extensions
openssl x509 -req -days "$DAYS_VALID" -in "$CERT_DIR/client.csr" \
    -CA "$CERT_DIR/ca.pem" -CAkey "$CERT_DIR/ca.key" \
    -CAcreateserial -out "$CERT_DIR/client.pem" \
    -sha256 -extfile "$CERT_DIR/client_ext.cnf"

# Generate server private key (ECDSA P-256 for DTLS compatibility) in PKCS#8 format
openssl ecparam -genkey -name prime256v1 | openssl pkcs8 -topk8 -nocrypt -out "$CERT_DIR/server.key"

# Generate server certificate request
openssl req -new -key "$CERT_DIR/server.key" -out "$CERT_DIR/server.csr" \
    -subj "/C=$COUNTRY/ST=$STATE/L=$CITY/O=$ORG/CN=localhost"

# Create extensions file for server certificate with SAN
cat > "$CERT_DIR/server_ext.cnf" <<EOF
basicConstraints = CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = DNS:localhost,IP:127.0.0.1,IP:::1
EOF

# Sign server certificate with CA including SAN extension
openssl x509 -req -days "$DAYS_VALID" -in "$CERT_DIR/server.csr" \
    -CA "$CERT_DIR/ca.pem" -CAkey "$CERT_DIR/ca.key" \
    -CAcreateserial -out "$CERT_DIR/server.pem" \
    -sha256 -extfile "$CERT_DIR/server_ext.cnf"

# Clean up CSR and extension files
rm -f "$CERT_DIR/client.csr" "$CERT_DIR/server.csr" "$CERT_DIR/server_ext.cnf" "$CERT_DIR/client_ext.cnf"

echo "Test certificates generated successfully!"
echo "Files created:"
echo "  - $CERT_DIR/ca.key     (CA private key)"
echo "  - $CERT_DIR/ca.pem     (CA certificate)"
echo "  - $CERT_DIR/ca.srl     (CA serial number file)"
echo "  - $CERT_DIR/server.key (Server private key)"
echo "  - $CERT_DIR/server.pem (Server certificate)"
echo "  - $CERT_DIR/client.key (Client private key)"
echo "  - $CERT_DIR/client.pem (Client certificate)"
echo ""
echo "These certificates are for testing only and should NOT be used in production."