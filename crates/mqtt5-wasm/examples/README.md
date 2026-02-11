# WASM MQTT Examples

This directory contains browser examples demonstrating the mqtt5-wasm library with full client and broker functionality.

## Use Cases

### External Broker (websocket/)

Connect to remote MQTT brokers via WebSocket.

### In-Tab Broker (local-broker/)

MQTT broker running in a browser tab using MessagePort.

### Broker Bridge (broker-bridge/)

Two in-browser brokers connected via MessagePort bridge, demonstrating bidirectional message forwarding.

### Shared Subscription (shared-subscription/)

Demonstrates MQTT v5.0 shared subscriptions for load balancing. Multiple workers subscribe to `$share/group/topic` and messages are distributed round-robin among them.

### Auth Tools (auth-tools/)

Browser-based tools for generating password and ACL files for native broker authentication. Uses the same Argon2 hashing as the CLI.

### QoS 2 Testing (qos2/)

Demonstrates QoS 2 (exactly once) message delivery with full acknowledgment flow.

### QoS 2 Recovery (qos2-recovery/)

Demonstrates QoS 2 mid-flight recovery after client disconnection. Subscriber disconnects during a QoS 2 exchange, then reconnects with cleanStart=false to receive the in-flight message from persistent session state.

### Will Message (will-message/)

Demonstrates Last Will and Testament (LWT) functionality for detecting unexpected client disconnections.

### Google JWT Auth (google-jwt-auth/)

Federated JWT authentication with Google OAuth. Demonstrates identity-only auth mode with ACL-based authorization.

### Request/Response RPC (request-response/)

Demonstrates MQTT v5.0 request/response pattern using ResponseTopic and CorrelationData for RPC-style communication.

### Retained Messages (retained-messages/)

Shows retained message functionality - late subscribers immediately receive the last retained message on a topic.

### Topic Aliases (topic-aliases/)

Demonstrates bandwidth optimization using topic aliases to reduce packet size for frequently-used topics.

### Session Recovery (session-recovery/)

Shows session persistence across reconnections with cleanStart=false and sessionExpiryInterval.

### $SYS Monitoring (sys-monitoring/)

Live broker metrics dashboard subscribing to $SYS/# topics for statistics.

### Message Expiry (message-expiry/)

Demonstrates message expiry intervals (TTL) - messages automatically expire after specified time.

### Flow Control (flow-control/)

Shows receiveMaximum-based flow control and backpressure handling for QoS 1/2 messages.

### BroadcastChannel (broadcast-channel/)

Cross-tab communication using BroadcastChannel transport for multi-tab MQTT coordination.

### Subscription Identifiers (subscription-ids/)

Demonstrates subscription identifiers for routing messages based on which subscription matched.

### ACL Permissions (acl-permissions/)

Shows ACL rule enforcement with visual feedback for allowed/denied operations.

### Rapid Ports (rapid-ports/)

Quick testing tool for MessagePort-based broker connections.

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

#### Broker Bridge

Two brokers connected via MessagePort bridge with bidirectional message forwarding.

```bash
cd broker-bridge
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### Shared Subscription

Demonstrates shared subscriptions for load balancing across workers.

```bash
cd shared-subscription
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### Auth Tools

Generate password and ACL files for broker authentication.

```bash
cd auth-tools
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### QoS 2 Testing

Test QoS 2 exactly-once message delivery.

```bash
cd qos2
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### QoS 2 Recovery

QoS 2 mid-flight recovery after disconnection.

```bash
cd qos2-recovery
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### Will Message

Test Last Will and Testament functionality.

```bash
cd will-message
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### Google JWT Auth

Federated authentication with Google OAuth.

```bash
cd google-jwt-auth
./run.sh
```

Open http://localhost:8000 in your browser.

#### Rapid Ports

Quick MessagePort connection testing.

```bash
cd rapid-ports
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### Request/Response RPC

RPC-style communication with correlation tracking.

```bash
cd request-response
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### Retained Messages

Late subscriber receives retained state immediately.

```bash
cd retained-messages
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### Topic Aliases

Bandwidth optimization demonstration.

```bash
cd topic-aliases
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### Session Recovery

Session persistence across reconnections.

```bash
cd session-recovery
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### $SYS Monitoring

Live broker metrics dashboard.

```bash
cd sys-monitoring
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### Message Expiry

Message TTL and expiration tracking.

```bash
cd message-expiry
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### Flow Control

Backpressure and receiveMaximum demonstration.

```bash
cd flow-control
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### BroadcastChannel

Cross-tab MQTT communication.

```bash
cd broadcast-channel
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### Subscription Identifiers

Message routing by subscription ID.

```bash
cd subscription-ids
python3 -m http.server 8000
```

Open http://localhost:8000 in your browser.

#### ACL Permissions

Permission enforcement visualization.

```bash
cd acl-permissions
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

### Broker Bridge Features

```javascript
import init, {
  WasmBroker,
  WasmBridgeConfig,
  WasmBridgeDirection,
  WasmTopicMapping
} from "./pkg/mqtt5_wasm.js";

await init();

const brokerA = new WasmBroker();
const brokerB = new WasmBroker();

const bridgeConfig = new WasmBridgeConfig("a-to-b");

const topicMapping = new WasmTopicMapping("sensors/#", WasmBridgeDirection.Both);
topicMapping.qos = 1;
bridgeConfig.add_topic(topicMapping);

const bridgePort = brokerB.create_client_port();
await brokerA.add_bridge(bridgeConfig, bridgePort);
```

The broker bridge:

- Connects two in-browser brokers via MessagePort
- Supports In, Out, and Both (bidirectional) directions
- Topic filtering with wildcard patterns
- QoS level configuration per topic mapping
- Optional local and remote topic prefixes

### Shared Subscription Features

```javascript
import init, { WasmBroker, WasmBrokerConfig, WasmMqttClient } from "./pkg/mqtt5_wasm.js";

await init();

const config = new WasmBrokerConfig();
config.shared_subscription_available = true;

const broker = WasmBroker.with_config(config);

const worker1 = new WasmMqttClient("worker-1");
const worker2 = new WasmMqttClient("worker-2");

const port1 = broker.create_client_port();
await worker1.connect_message_port(port1);

const port2 = broker.create_client_port();
await worker2.connect_message_port(port2);

await worker1.subscribe_with_callback("$share/workers/tasks/+", (topic, payload) => {
  console.log("Worker 1 received:", topic);
});

await worker2.subscribe_with_callback("$share/workers/tasks/+", (topic, payload) => {
  console.log("Worker 2 received:", topic);
});
```

Shared subscriptions:

- Topic format: `$share/{group-name}/{topic-filter}`
- Messages distributed round-robin among group members
- Each message delivered to exactly one subscriber in the group
- Multiple groups can exist for the same topic
- Regular and shared subscriptions can coexist

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
- **MessagePort bridging only**: Broker-to-broker connections use MessagePort, not network sockets (see broker-bridge example)
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
