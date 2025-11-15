#!/bin/bash
set -e

echo "Building WASM package..."
cd "$(dirname "$0")/../.."

wasm-pack build --target web --features wasm-client

echo "Copying to example directories..."
cp -r pkg examples/wasm/websocket/
cp -r pkg examples/wasm/message-port/
cp -r pkg examples/wasm/broadcast-channel/

echo "✨ Build complete!"
echo ""
echo "Available examples:"
echo ""
echo "WebSocket (external broker):"
echo "  cd examples/wasm/websocket"
echo "  python3 -m http.server 8000"
echo ""
echo "MessagePort (Service Worker proxy):"
echo "  cd examples/wasm/message-port"
echo "  python3 -m http.server 8001"
echo ""
echo "BroadcastChannel (in-memory broker):"
echo "  cd examples/wasm/broadcast-channel"
echo "  python3 -m http.server 8002"
echo ""
echo "Then open http://localhost:800X in your browser"
