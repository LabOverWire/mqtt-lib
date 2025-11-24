# WASM MQTT Examples

This directory contains browser examples demonstrating the mqtt5-wasm library with full client and broker functionality.

## Use Cases

### External Broker (websocket/)

Connect to remote MQTT brokers via WebSocket.

### In-Tab Broker (local-broker/)

MQTT broker running in a browser tab using MessagePort.

## Quick Start

### 1. Build the WASM Package

From the repository root:

```bash
cd crates/mqtt5-wasm/examples
./build.sh
```

This will:

- Build the WASM package with `wasm-pack`
- Copy it to example directories
- Display instructions for running examples

### 2. Run an Example

#### WebSocket (External Broker)

Connects to a remote MQTT broker over WebSocket.

```bash
cd websocket
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### Local Broker (In-Tab)

Runs a complete MQTT broker in your browser tab.

```bash
cd local-broker
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

## Features Demonstrated

### Local Broker Features

```javascript
import init, { WasmBroker, WasmMqttClient } from "./pkg/mqtt5_wasm.js";

await init();

const broker = new WasmBroker();
const client = new WasmMqttClient("local-client");

const port = broker.create_client_port();
await client.connect_message_port(port);

await client.subscribe_with_callback("test/topic", (topic, payload) => {
  console.log("Message received:", topic, payload);
});

await client.publish("test/topic", encoder.encode("Hello"));
```

The in-tab broker:

- Runs entirely in your browser (no external dependencies)
- Uses MessagePort for client-broker communication
- Memory-only storage (no persistence)
- No authentication (AllowAllAuthProvider)
- Supports all core MQTT v5.0 features (QoS, retained messages, subscriptions)

### Connection Events

All examples demonstrate the event callback system:

```javascript
client.on_connect((reasonCode, sessionPresent) => {
  console.log("Connected!", reasonCode, sessionPresent);
});

client.on_disconnect(() => {
  console.log("Disconnected");
});

client.on_error((error) => {
  console.error("Error:", error);
});
```

### Message Callbacks

Subscribe with automatic message handling:

```javascript
await client.subscribe_with_callback("test/topic", (topic, payload) => {
  const decoder = new TextDecoder();
  const message = decoder.decode(payload);
  console.log("Received:", topic, message);
});
```

### Unsubscribe

Remove subscriptions dynamically:

```javascript
await client.unsubscribe("test/topic");
```

### Publishing

QoS 0 (fire-and-forget):

```javascript
const encoder = new TextEncoder();
await client.publish("test/topic", encoder.encode("Hello"));
```

### Automatic Keepalive

The client automatically:

- Sends PINGREQ packets every 30 seconds
- Detects connection timeout after 90 seconds
- Triggers `on_error` and `on_disconnect` callbacks on timeout

## Testing

### Testing Keepalive

1. Connect to a broker
2. Open browser DevTools console
3. Watch for "PINGRESP received" messages every ~30 seconds
4. Stop the broker or disconnect network
5. See connection timeout after 90 seconds
6. Observe `on_error("Keepalive timeout")` and `on_disconnect()` callbacks

### Testing Error Handling

1. Try connecting to an invalid URL
2. Observe `on_error` callback with connection error
3. Try publishing while disconnected
4. See JavaScript error alerts

## Browser Compatibility

- Chrome/Edge 90+
- Firefox 88+
- Safari 15.4+

## WASM Limitations

The WASM build has the following constraints compared to the native Rust library:

### Transport Limitations

- **No TLS support**: Browser security model prevents raw TLS socket access
- **WebSocket only for external brokers**: Use `ws://` or `wss://` (browser-managed TLS)
- **MessagePort for in-tab broker**: Communication within the same browser tab

### Storage Limitations

- **Memory-only storage**: No file persistence available in browser environment
- **Session data lost on page reload**: All broker state is transient

### Network Limitations

- **No server sockets**: Cannot listen for incoming TCP/TLS connections
- **No broker bridging**: Network-based broker-to-broker connections unavailable
- **No file-based configuration**: All configuration must be done programmatically

These limitations are inherent to the browser sandbox security model. For production MQTT deployments, use the native Rust library.

## Troubleshooting

**WASM fails to load:**

- Ensure you're using a web server (not `file://`)
- Check that `pkg/` directory exists with `mqtt5_bg.wasm`
- Verify MIME type: server should send `.wasm` as `application/wasm`

**Connection fails:**

- Check broker URL format: `ws://` or `wss://`
- Verify broker is accessible (test with another MQTT client)
- Check browser console for CORS errors
- Try a public broker: `ws://broker.hivemq.com:8000/mqtt`

**Messages not received:**

- Ensure you used `subscribe_with_callback()`, not `subscribe()`
- Check browser console for callback errors
- Verify topic matches (wildcards: `+` for single level, `#` for multi-level)

**Keepalive timeout:**

- This is expected if broker becomes unreachable
- Check network connectivity
- Verify broker supports MQTT v5.0

## Public Test Brokers

Free brokers for testing (no authentication):

- HiveMQ: `ws://broker.hivemq.com:8000/mqtt`
- Mosquitto: `ws://test.mosquitto.org:8080`
- EMQX: `ws://broker.emqx.io:8083/mqtt`

**Note:** Public brokers are shared - use unique topics to avoid conflicts.

## Next Steps

- See the main README for complete API documentation
- Check `crates/mqtt5-wasm/src/client.rs` for implementation details
- Review the native client examples in `crates/mqtt5/examples/` for comparison
