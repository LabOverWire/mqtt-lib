# WASM MQTT Client Examples

This directory contains browser examples demonstrating the mqtt5 WASM client with all its features including message callbacks, connection events, and QoS support.

## Quick Start

### 1. Build the WASM Package

From the repository root:

```bash
cd examples/wasm
./build.sh
```

This will:
- Build the WASM package with `wasm-pack`
- Copy it to all example directories
- Display instructions for running each example

### 2. Run an Example

Choose one of the three transport types:

#### WebSocket (External Broker)

```bash
cd websocket
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### MessagePort (Service Worker)

```bash
cd message-port
python3 -m http.server 8001
```

Open http://localhost:8001 in your browser.

#### BroadcastChannel (In-Browser P2P)

```bash
cd broadcast-channel
python3 -m http.server 8002
```

Open http://localhost:8002 in your browser.

## Features Demonstrated

### Connection Events

All examples demonstrate the event callback system:

```javascript
client.on_connect((reasonCode, sessionPresent) => {
  console.log('Connected!', reasonCode, sessionPresent);
});

client.on_disconnect(() => {
  console.log('Disconnected');
});

client.on_error((error) => {
  console.error('Error:', error);
});
```

### Message Callbacks

Subscribe with automatic message handling:

```javascript
await client.subscribe_with_callback('test/topic', (topic, payload) => {
  const decoder = new TextDecoder();
  const message = decoder.decode(payload);
  console.log('Received:', topic, message);
});
```

### Unsubscribe

Remove subscriptions dynamically:

```javascript
await client.unsubscribe('test/topic');
```

### Publishing

QoS 0 (fire-and-forget):

```javascript
const encoder = new TextEncoder();
await client.publish('test/topic', encoder.encode('Hello'));
```

### Automatic Keepalive

The client automatically:
- Sends PINGREQ packets every 30 seconds
- Detects connection timeout after 90 seconds
- Triggers `on_error` and `on_disconnect` callbacks on timeout

## Testing

### WebSocket Example

1. Start the example server
2. Connect to a public broker (default: `ws://broker.hivemq.com:8000/mqtt`)
3. Subscribe to a topic (e.g., `test/mqtt5-wasm`)
4. Open the same URL in another browser tab
5. Publish messages between tabs

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

- See `docs/WASM_USAGE.md` for complete API documentation
- Check `src/wasm/client.rs` for implementation details
- Review the native client examples in `examples/` for comparison
