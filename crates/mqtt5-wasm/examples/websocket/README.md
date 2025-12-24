# MQTT WebSocket WASM Example

This example demonstrates the mqtt5 library running as WebAssembly in the browser, connecting to an MQTT broker via WebSocket.

## Features

This example demonstrates all MQTT v5.0 configuration options available in the WASM client:

### Connection Features
- WebSocket transport to MQTT brokers
- Connection options: keepAlive, cleanStart, sessionExpiryInterval, receiveMaximum, maximumPacketSize
- Will message support with QoS, retain, delay interval, and message expiry
- User properties on connection (client-type, client-version, example)

### Publish Features
- QoS levels (0, 1, 2) with configurable options
- Retain flag for persistent messages
- Message properties: messageExpiryInterval, payloadFormatIndicator, contentType
- User properties on publish (sender, timestamp)

### Subscribe Features
- QoS selection (0, 1, 2)
- Topic wildcards (`+` single-level, `#` multi-level)
- Subscription options: noLocal, retainAsPublished, retainHandling
- Subscription identifiers for message routing
- Dynamic subscription management (subscribe/unsubscribe)

### Message Handling
- Real-time message display
- Connection event callbacks (connect, disconnect, error)
- Status monitoring with will messages

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
cp -r pkg crates/mqtt5-wasm/examples/websocket/
```

Or use the provided build script:

```bash
./crates/mqtt5-wasm/examples/build.sh
```

## Running

Start a local HTTP server in the example directory:

### Using Python 3:
```bash
cd crates/mqtt5-wasm/examples/websocket
python3 -m http.server 8000
```

### Using Node.js (with http-server):
```bash
cd crates/mqtt5-wasm/examples/websocket
npx http-server -p 8000
```

Then open your browser to: http://localhost:8000

## Usage

### Connecting to a Broker

1. Enter the WebSocket URL of your MQTT broker
   - HiveMQ: `wss://broker.hivemq.com:8884/mqtt` (public test broker, secure)
   - HiveMQ: `ws://broker.hivemq.com:8000/mqtt` (public test broker, unsecure)
   - Mosquitto: `ws://test.mosquitto.org:8080/mqtt` (public test broker)
   - EMQX: `ws://broker.emqx.io:8083/mqtt` (public test broker, unsecure)
   - EMQX: `wss://broker.emqx.io:8084/mqtt` (public test broker, secure)
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

## Configuration Examples

The example demonstrates all available configuration options:

### Connection with Options

```javascript
import { WasmMqttClient, WasmConnectOptions, WasmWillMessage } from './pkg/mqtt5_wasm.js';

const client = new WasmMqttClient("my-client-id");

const connectOpts = new WasmConnectOptions();
connectOpts.keepAlive = 60;
connectOpts.cleanStart = true;
connectOpts.sessionExpiryInterval = 3600;
connectOpts.receiveMaximum = 100;
connectOpts.maximumPacketSize = 131072;

connectOpts.addUserProperty("client-type", "browser");
connectOpts.addUserProperty("client-version", "0.10.0");

const will = new WasmWillMessage("status/offline", encoder.encode("offline"));
will.qos = 1;
will.retain = true;
will.willDelayInterval = 5;
will.messageExpiryInterval = 300;
connectOpts.set_will(will);

await client.connect_with_options("ws://broker:8000/mqtt", connectOpts);
```

### Publish with Options

```javascript
import { WasmPublishOptions } from './pkg/mqtt5_wasm.js';

const pubOpts = new WasmPublishOptions();
pubOpts.qos = 1;
pubOpts.retain = false;
pubOpts.messageExpiryInterval = 300;
pubOpts.payloadFormatIndicator = true;
pubOpts.contentType = "text/plain";
pubOpts.addUserProperty("sender", "websocket-example");

const encoder = new TextEncoder();
await client.publish_with_options("topic", encoder.encode("message"), pubOpts);
```

### Subscribe with Options

```javascript
import { WasmSubscribeOptions } from './pkg/mqtt5_wasm.js';

const subOpts = new WasmSubscribeOptions();
subOpts.qos = 1;
subOpts.noLocal = false;
subOpts.retainAsPublished = true;
subOpts.retainHandling = 0;
subOpts.subscriptionIdentifier = 42;

await client.subscribe_with_options("topic/filter", (topic, payload) => {
    console.log('Message:', topic, payload);
}, subOpts);
```

## Architecture

This example demonstrates:

1. **WASM Initialization**: Loading the mqtt5 WebAssembly module
2. **WebSocket Transport**: Using the browser's native WebSocket API
3. **Async/Await**: Rust async functions exposed to JavaScript as Promises
4. **Binary Data**: Handling MQTT binary payloads with Uint8Array
5. **Client State Management**: Connection lifecycle and message handling
6. **Full MQTT v5.0 Configuration**: All connection, publish, and subscribe options

## Next Steps

- Try the [Local Broker example](../local-broker/) for in-browser MQTT broker
- Try the [Broker Bridge example](../broker-bridge/) for MessagePort connections
- Explore the [source code](../../../src/wasm/client.rs) for the WASM client implementation

## License

Same as the mqtt5 library: MIT OR Apache-2.0
