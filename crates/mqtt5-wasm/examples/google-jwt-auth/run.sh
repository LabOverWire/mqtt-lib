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
    echo "║     MQTT Federated JWT Auth - Google Test (WASM Client)       ║"
    echo "╚═══════════════════════════════════════════════════════════════╝"
    echo ""
}

check_dependencies() {
    local missing=()
    command -v openssl &>/dev/null || missing+=("openssl")
    command -v python3 &>/dev/null || missing+=("python3")
    command -v wasm-pack &>/dev/null || missing+=("wasm-pack")

    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "Error: Missing dependencies: ${missing[*]}"
        if [[ " ${missing[*]} " == *" wasm-pack "* ]]; then
            echo ""
            echo "Install wasm-pack with:"
            echo "  curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh"
            echo "  or: cargo install wasm-pack"
        fi
        exit 1
    fi
}

build_wasm() {
    local pkg_dir="$SCRIPT_DIR/pkg"

    if [[ -f "$pkg_dir/mqtt5_wasm.js" ]]; then
        echo "Using existing WASM package at $pkg_dir/"
    else
        echo "Building mqtt5-wasm package..."
        cd "$REPO_ROOT/crates/mqtt5-wasm"
        wasm-pack build --target web --out-dir "$pkg_dir"
        echo "WASM package built successfully."
    fi
    echo ""
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
    echo "Starting HTTP server for WASM test app..."

    cat > "$KEYS_DIR/config.json" << EOF
{
  "clientId": "$GOOGLE_CLIENT_ID",
  "brokerUrl": "ws://localhost:8080/mqtt"
}
EOF

    cd "$SCRIPT_DIR"

    python3 << 'PYEOF' &
import http.server
import os
import json

CONFIG_PATH = os.environ.get("CONFIG_PATH", "/tmp/mqtt-jwt-test/config.json")

class WasmHandler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/config.json" or self.path.startswith("/config.json?"):
            try:
                self.send_response(200)
                self.send_header("Content-type", "application/json")
                self.send_header("Cache-Control", "no-cache, no-store, must-revalidate")
                self.end_headers()
                with open(CONFIG_PATH, "rb") as f:
                    self.wfile.write(f.read())
            except FileNotFoundError:
                self.send_response(404)
                self.end_headers()
        else:
            super().do_GET()

    def guess_type(self, path):
        if path.endswith('.wasm'):
            return 'application/wasm'
        return super().guess_type(path)

    def log_message(self, format, *args):
        pass

server = http.server.HTTPServer(("", 8000), WasmHandler)
print("Serving WASM app on port 8000")
server.serve_forever()
PYEOF

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
    echo "║  This example uses mqtt5-wasm (Rust compiled to WASM)         ║"
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
    build_wasm
    build_broker
    generate_keys
    get_client_id
    start_broker
    start_http_server
    print_instructions

    while true; do
        sleep 1
        if ! kill -0 "$BROKER_PID" 2>/dev/null; then
            echo "Broker stopped unexpectedly. Check $KEYS_DIR/broker.log"
            cleanup
        fi
    done
}

main "$@"
