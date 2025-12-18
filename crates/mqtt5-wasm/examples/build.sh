#!/bin/bash
set -e

echo "Building WASM package..."
cd "$(dirname "$0")/.."

wasm-pack build --target web --features client,broker

echo "Copying to example directories..."
cp -r pkg examples/websocket/
cp -r pkg examples/local-broker/
cp -r pkg examples/qos2/
cp -r pkg examples/broker-bridge/
cp -r pkg examples/will-message/
cp -r pkg examples/shared-subscription/
cp -r pkg examples/auth-tools/
cp -r pkg examples/google-jwt-auth/
cp -r pkg examples/rapid-ports/

echo "✨ Build complete!"
echo ""
echo "Available examples:"
echo ""
echo "WebSocket (external broker):"
echo "  cd examples/websocket"
echo "  python3 -m http.server 8000"
echo ""
echo "Local broker (in-tab):"
echo "  cd examples/local-broker"
echo "  python3 -m http.server 8000"
echo ""
echo "Broker Bridge (two brokers):"
echo "  cd examples/broker-bridge"
echo "  python3 -m http.server 8000"
echo ""
echo "QoS 2 Testing:"
echo "  cd examples/qos2"
echo "  python3 -m http.server 8000"
echo ""
echo "Will Message (Last Will and Testament):"
echo "  cd examples/will-message"
echo "  python3 -m http.server 8000"
echo ""
echo "Shared Subscription (Load Balancing):"
echo "  cd examples/shared-subscription"
echo "  python3 -m http.server 8000"
echo ""
echo "Auth Tools (Password & ACL Generator):"
echo "  cd examples/auth-tools"
echo "  python3 -m http.server 8000"
echo ""
echo "Google JWT Auth (Federated Authentication):"
echo "  cd examples/google-jwt-auth"
echo "  ./run.sh"
echo ""
echo "Rapid Ports (Quick Port Testing):"
echo "  cd examples/rapid-ports"
echo "  python3 -m http.server 8000"
echo ""
echo "Then open http://localhost:8000 in your browser"
