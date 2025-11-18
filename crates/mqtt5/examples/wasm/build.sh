#!/bin/bash
set -e

echo "Building WASM package..."
cd "$(dirname "$0")/../.."

wasm-pack build --target web --features wasm-client,wasm-broker

echo "Copying to example directories..."
cp -r pkg examples/wasm/websocket/
cp -r pkg examples/wasm/local-broker/
cp -r pkg examples/wasm/qos2/

echo "✨ Build complete!"
echo ""
echo "Available examples:"
echo ""
echo "WebSocket (external broker):"
echo "  cd examples/wasm/websocket"
echo "  python3 -m http.server 8000"
echo ""
echo "Local broker (in-tab):"
echo "  cd examples/wasm/local-broker"
echo "  python3 -m http.server 8000"
echo ""
echo "QoS 2 Testing:"
echo "  cd examples/wasm/qos2"
echo "  python3 -m http.server 8000"
echo ""
echo "Then open http://localhost:8000 in your browser"
