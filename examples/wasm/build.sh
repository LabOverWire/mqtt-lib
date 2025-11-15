#!/bin/bash
set -e

echo "Building WASM package..."
cd "$(dirname "$0")/../.."

wasm-pack build --target web --features wasm-client

echo "Copying to example directories..."
cp -r pkg examples/wasm/websocket/

echo "✨ Build complete!"
echo ""
echo "To run the WebSocket example:"
echo "  cd examples/wasm/websocket"
echo "  python3 -m http.server 8000"
echo ""
echo "Then open http://localhost:8000 in your browser"
