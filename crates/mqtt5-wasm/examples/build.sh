#!/bin/bash
set -e

echo "Building WASM package..."
cd "$(dirname "$0")/.."

wasm-pack build --target web --features client,broker,codec

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
cp -r pkg examples/request-response/
cp -r pkg examples/retained-messages/
cp -r pkg examples/topic-aliases/
cp -r pkg examples/session-recovery/
cp -r pkg examples/sys-monitoring/
cp -r pkg examples/message-expiry/
cp -r pkg examples/flow-control/
cp -r pkg examples/broadcast-channel/
cp -r pkg examples/subscription-ids/
cp -r pkg examples/acl-permissions/
cp -r pkg examples/codec-compression/
cp -r pkg examples/loop-prevention/

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
echo "Request/Response (RPC Pattern):"
echo "  cd examples/request-response"
echo "  python3 -m http.server 8000"
echo ""
echo "Retained Messages (Persistent State):"
echo "  cd examples/retained-messages"
echo "  python3 -m http.server 8000"
echo ""
echo "Topic Aliases (Bandwidth Optimization):"
echo "  cd examples/topic-aliases"
echo "  python3 -m http.server 8000"
echo ""
echo "Session Recovery (Reconnection Demo):"
echo "  cd examples/session-recovery"
echo "  python3 -m http.server 8000"
echo ""
echo "\$SYS Monitoring (Broker Dashboard):"
echo "  cd examples/sys-monitoring"
echo "  python3 -m http.server 8000"
echo ""
echo "Message Expiry (TTL Demo):"
echo "  cd examples/message-expiry"
echo "  python3 -m http.server 8000"
echo ""
echo "Flow Control (Backpressure Demo):"
echo "  cd examples/flow-control"
echo "  python3 -m http.server 8000"
echo ""
echo "BroadcastChannel (Cross-Tab Demo):"
echo "  cd examples/broadcast-channel"
echo "  python3 -m http.server 8000"
echo ""
echo "Subscription IDs (Message Routing):"
echo "  cd examples/subscription-ids"
echo "  python3 -m http.server 8000"
echo ""
echo "ACL Permissions (Permission Denial Demo):"
echo "  cd examples/acl-permissions"
echo "  python3 -m http.server 8000"
echo ""
echo "Codec Compression (Gzip/Deflate Demo):"
echo "  cd examples/codec-compression"
echo "  python3 -m http.server 8000"
echo ""
echo "Loop Prevention (Duplicate Blocking Demo):"
echo "  cd examples/loop-prevention"
echo "  python3 -m http.server 8000"
echo ""
echo "Then open http://localhost:8000 in your browser"
