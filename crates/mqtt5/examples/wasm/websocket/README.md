# MQTT WebSocket WASM Example

This example demonstrates the mqtt5 library running as WebAssembly in the browser, connecting to an MQTT broker via WebSocket.

## Features

- Connect to any MQTT broker supporting WebSocket
- Subscribe to topics with wildcard support
- Publish messages to topics
- View received messages in real-time
- Clean, modern UI

## Prerequisites

- [wasm-pack](https://rustwasm.github.io/wasm-pack/installer/) installed
- Modern web browser (Chrome, Firefox, Safari, Edge)
- Simple HTTP server (Python, Node.js, or similar)

## Building

From the repository root:

```bash
# Build the WASM package
wasm-pack build --target web --features wasm-client

# Copy the output to the example directory
cp -r pkg crates/mqtt5/examples/wasm/websocket/
```

Or use the provided build script:

```bash
./crates/mqtt5/examples/wasm/build.sh
```

## Running

Start a local HTTP server in the example directory:

### Using Python 3:
```bash
cd crates/mqtt5/examples/wasm/websocket
python3 -m http.server 8000
```

### Using Node.js (with http-server):
```bash
cd crates/mqtt5/examples/wasm/websocket
npx http-server -p 8000
```

Then open your browser to: http://localhost:8000

## Usage

### Connecting to a Broker

1. Enter the WebSocket URL of your MQTT broker
   - Default: `ws://test.mosquitto.org:8080/mqtt` (public test broker)
   - For local broker: `ws://localhost:8080/mqtt` (if your broker supports WebSocket)

2. Optionally enter a Client ID (auto-generated if left empty)

3. Click **Connect**

### Subscribing to Topics

1. Once connected, enter a topic filter:
   - Exact topic: `test/topic`
   - Single-level wildcard: `sensors/+/temperature`
   - Multi-level wildcard: `home/#`

2. Click **Subscribe**

3. You'll see the subscription appear in the list below

### Publishing Messages

1. Enter a topic name (no wildcards for publish)

2. Enter your message payload

3. Click **Publish**

4. The message will appear in the messages log as "sent"

### Viewing Messages

- All received messages appear in the Messages section
- Messages show the topic, timestamp, and payload
- Click **Clear** to remove all messages from the log

## Testing with Public Broker

The default broker `ws://test.mosquitto.org:8080/mqtt` is a public test broker:

1. Open two browser tabs with the example
2. In Tab 1: Subscribe to `test/wasm`
3. In Tab 2: Publish to `test/wasm` with message "Hello from WASM!"
4. Tab 1 should receive the message

## Using Your Own Broker

### Mosquitto Example

Add WebSocket listener to `mosquitto.conf`:
```
listener 1883
listener 8080
protocol websockets
```

Restart Mosquitto and connect to `ws://localhost:8080/mqtt`

### EMQX Example

EMQX enables WebSocket by default on port 8083:
```
ws://localhost:8083/mqtt
```

## Browser Compatibility

- ✅ Chrome/Edge (v90+)
- ✅ Firefox (v88+)
- ✅ Safari (v15+)

## Troubleshooting

### "Failed to initialize WASM"
- Ensure you built the WASM package (`wasm-pack build`)
- Ensure `pkg/` directory exists in this folder
- Check browser console for detailed errors

### "Connection failed"
- Verify the broker URL is correct and uses `ws://` or `wss://` protocol
- Ensure the broker supports WebSocket connections
- Check if the broker is reachable (try telnet/nc to the WebSocket port)
- Some brokers require a specific path (e.g., `/mqtt`)

### "Mixed Content" Error (HTTPS page + ws://)
- Use `wss://` for secure WebSocket connections
- Or serve the page via `http://` instead of `https://`

### CORS Issues
- MQTT over WebSocket typically doesn't have CORS issues
- Ensure you're serving the page from an HTTP server (not `file://`)

## Architecture

This example demonstrates:

1. **WASM Initialization**: Loading the mqtt5 WebAssembly module
2. **WebSocket Transport**: Using the browser's native WebSocket API
3. **Async/Await**: Rust async functions exposed to JavaScript as Promises
4. **Binary Data**: Handling MQTT binary payloads with Uint8Array
5. **Client State Management**: Connection lifecycle and message handling

## Next Steps

- Try the [MessagePort example](../message-port/) for Service Worker-based broker
- Try the [BroadcastChannel example](../broadcast-channel/) for cross-tab messaging
- Explore the [source code](../../../src/wasm/client.rs) for the WASM client implementation

## License

Same as the mqtt5 library: MIT OR Apache-2.0
