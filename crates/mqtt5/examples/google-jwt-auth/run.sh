#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
BROKER_BIN="$REPO_ROOT/target/release/mqttv5"
KEYS_DIR="/tmp/mqtt-jwt-test"
BROKER_PID=""
HTTP_PID=""

cleanup() {
    echo ""
    echo "Shutting down..."
    [[ -n "$BROKER_PID" ]] && kill "$BROKER_PID" 2>/dev/null || true
    [[ -n "$HTTP_PID" ]] && kill "$HTTP_PID" 2>/dev/null || true
    exit 0
}

trap cleanup SIGINT SIGTERM

print_banner() {
    echo "╔═══════════════════════════════════════════════════════════════╗"
    echo "║         MQTT Federated JWT Auth - Google Test                 ║"
    echo "╚═══════════════════════════════════════════════════════════════╝"
    echo ""
}

check_dependencies() {
    local missing=()
    command -v openssl &>/dev/null || missing+=("openssl")
    command -v python3 &>/dev/null || missing+=("python3")

    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "Error: Missing dependencies: ${missing[*]}"
        exit 1
    fi
}

build_broker() {
    if [[ ! -f "$BROKER_BIN" ]]; then
        echo "Building broker (release)..."
        cd "$REPO_ROOT"
        cargo build --release -p mqttv5-cli
        echo "Build complete."
        echo ""
    fi
}

generate_keys() {
    mkdir -p "$KEYS_DIR"

    if [[ ! -f "$KEYS_DIR/fallback-public.pem" ]]; then
        echo "Generating fallback RSA key pair..."
        openssl genrsa -out "$KEYS_DIR/fallback-private.pem" 2048 2>/dev/null
        openssl rsa -in "$KEYS_DIR/fallback-private.pem" -pubout -out "$KEYS_DIR/fallback-public.pem" 2>/dev/null
        echo "Keys generated at $KEYS_DIR/"
    else
        echo "Using existing keys at $KEYS_DIR/"
    fi
    echo ""
}

get_client_id() {
    if [[ -n "$GOOGLE_CLIENT_ID" ]]; then
        echo "Using GOOGLE_CLIENT_ID from environment"
        return
    fi

    if [[ -f "$KEYS_DIR/client_id.txt" ]]; then
        GOOGLE_CLIENT_ID=$(cat "$KEYS_DIR/client_id.txt")
        echo "Using saved Google Client ID"
        return
    fi

    echo "┌─────────────────────────────────────────────────────────────────┐"
    echo "│ Google OAuth Client ID Required                                 │"
    echo "├─────────────────────────────────────────────────────────────────┤"
    echo "│ 1. Go to: https://console.cloud.google.com/apis/credentials     │"
    echo "│ 2. Create OAuth 2.0 Client ID (Web application)                 │"
    echo "│ 3. Add authorized origin: http://localhost:8000                 │"
    echo "│ 4. Copy the Client ID                                           │"
    echo "└─────────────────────────────────────────────────────────────────┘"
    echo ""
    read -p "Enter Google Client ID: " GOOGLE_CLIENT_ID

    if [[ -z "$GOOGLE_CLIENT_ID" ]]; then
        echo "Error: Client ID is required"
        exit 1
    fi

    echo "$GOOGLE_CLIENT_ID" > "$KEYS_DIR/client_id.txt"
    echo ""
}

start_broker() {
    echo "Starting MQTT broker..."
    echo "  - JWT Issuer: https://accounts.google.com"
    echo "  - JWKS URI: https://www.googleapis.com/oauth2/v3/certs"
    echo "  - WebSocket: ws://localhost:8080/mqtt"
    echo ""

    RUST_LOG=mqtt5=debug "$BROKER_BIN" broker \
        --auth-method jwt-federated \
        --jwt-issuer "https://accounts.google.com" \
        --jwt-jwks-uri "https://www.googleapis.com/oauth2/v3/certs" \
        --jwt-fallback-key "$KEYS_DIR/fallback-public.pem" \
        --jwt-audience "$GOOGLE_CLIENT_ID" \
        --ws-host 0.0.0.0:8080 \
        --non-interactive \
        > "$KEYS_DIR/broker.log" 2>&1 &

    BROKER_PID=$!

    sleep 2

    if ! kill -0 "$BROKER_PID" 2>/dev/null; then
        echo "Error: Broker failed to start. Check $KEYS_DIR/broker.log"
        cat "$KEYS_DIR/broker.log"
        exit 1
    fi

    echo "Broker started (PID: $BROKER_PID)"
    echo ""
}

start_http_server() {
    echo "Starting HTTP server for test app..."

    cat > "$KEYS_DIR/config.json" << EOF
{
  "clientId": "$GOOGLE_CLIENT_ID",
  "brokerUrl": "ws://localhost:8080/mqtt"
}
EOF

    cd "$SCRIPT_DIR"
    CONFIG_PATH="$KEYS_DIR/config.json" python3 "$SCRIPT_DIR/server.py" 8000 &
    HTTP_PID=$!
    sleep 1
    echo "HTTP server started (PID: $HTTP_PID)"
    echo ""
}

print_instructions() {
    echo "╔═══════════════════════════════════════════════════════════════╗"
    echo "║  Ready! Open in browser:                                      ║"
    echo "║                                                               ║"
    echo "║    http://localhost:8000                                      ║"
    echo "║                                                               ║"
    echo "╠═══════════════════════════════════════════════════════════════╣"
    echo "║  Steps:                                                       ║"
    echo "║  1. Sign in with Google                                       ║"
    echo "║  2. Click 'Connect with JWT'                                  ║"
    echo "║  3. Subscribe/Publish to test                                 ║"
    echo "╠═══════════════════════════════════════════════════════════════╣"
    echo "║  Logs:                                                        ║"
    echo "║    Broker: tail -f $KEYS_DIR/broker.log"
    echo "╠═══════════════════════════════════════════════════════════════╣"
    echo "║  Press Ctrl+C to stop                                         ║"
    echo "╚═══════════════════════════════════════════════════════════════╝"
    echo ""
}

main() {
    print_banner
    check_dependencies
    build_broker
    generate_keys
    get_client_id
    start_broker
    start_http_server
    print_instructions

    # Wait for interrupt
    while true; do
        sleep 1
        # Check if processes are still running
        if ! kill -0 "$BROKER_PID" 2>/dev/null; then
            echo "Broker stopped unexpectedly. Check $KEYS_DIR/broker.log"
            cleanup
        fi
    done
}

main "$@"
