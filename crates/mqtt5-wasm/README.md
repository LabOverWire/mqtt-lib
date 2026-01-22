# mqtt5-wasm

MQTT v5.0 and v3.1.1 WebAssembly client and broker for browser environments.

## Features

- **WebSocket transport** - Connect to remote MQTT brokers via `ws://` or `wss://`
- **In-tab broker** - Run a complete MQTT broker in the browser
- **MessagePort/BroadcastChannel** - Inter-tab and worker communication
- **Broker bridging** - Connect multiple in-browser brokers via MessagePort
- **Full QoS support** - QoS 0, 1, and 2 with proper acknowledgment
- **Shared subscriptions** - Load balancing across multiple subscribers
- **Event callbacks** - Connection, disconnect, and error event handlers
- **Broker lifecycle events** - Monitor client connections, publishes, and subscriptions
- **Automatic keepalive** - Connection health monitoring with timeout detection
- **Will messages** - Last Will and Testament (LWT) support

## Installation

### npm (browser/bundler)

```bash
npm install mqtt5-wasm
```

### Cargo (Rust)

```toml
[dependencies]
mqtt5-wasm = "0.10"
```

Build with wasm-bindgen:

```bash
cargo build --target wasm32-unknown-unknown --release --features client,broker,codec
wasm-bindgen --target web --out-dir pkg target/wasm32-unknown-unknown/release/mqtt5_wasm.wasm
```

## Usage

### Basic Example

```javascript
import init, { WasmMqttClient } from "mqtt5-wasm";

await init();
const client = new WasmMqttClient("browser-client");

await client.connect("wss://broker.example.com:8084/mqtt");

await client.subscribe_with_callback("sensors/#", (topic, payload) => {
  console.log(`${topic}: ${new TextDecoder().decode(payload)}`);
});

await client.publish("sensors/temp", new TextEncoder().encode("25.5"));

await client.disconnect();
```

### Event Callbacks

```javascript
const client = new WasmMqttClient("browser-client");

client.on_connect((reasonCode, sessionPresent) => {
  console.log(`Connected: reason=${reasonCode}, session=${sessionPresent}`);
});

client.on_disconnect(() => {
  console.log("Disconnected from broker");
});

client.on_error((error) => {
  console.error(`Error: ${error}`);
});

await client.connect("wss://broker.example.com:8084/mqtt");
```

### In-Browser Broker

```javascript
import init, { WasmBroker, WasmMqttClient } from "mqtt5-wasm";

await init();

const broker = new WasmBroker();
const port = broker.create_client_port();

const client = new WasmMqttClient("local-client");
await client.connect_message_port(port);
```

### Broker Lifecycle Events

Monitor broker activity with lifecycle callbacks:

```javascript
const broker = new WasmBroker();

broker.on_client_connect((event) => {
  console.log(`Client connected: ${event.clientId}, cleanStart: ${event.cleanStart}`);
});

broker.on_client_disconnect((event) => {
  console.log(`Client disconnected: ${event.clientId}, reason: ${event.reason}`);
});

broker.on_client_publish((event) => {
  console.log(`Publish: ${event.topic} (${event.payloadSize} bytes, QoS ${event.qos})`);
});

broker.on_client_subscribe((event) => {
  console.log(`Subscribe: ${event.clientId} -> ${event.subscriptions.map(s => s.topic)}`);
});

broker.on_client_unsubscribe((event) => {
  console.log(`Unsubscribe: ${event.clientId} -> ${event.topics}`);
});

broker.on_message_delivered((event) => {
  console.log(`Message delivered: packetId=${event.packetId}, QoS ${event.qos}`);
});
```

## Documentation

See the [main repository](https://github.com/LabOverWire/mqtt-lib) for complete documentation and examples.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
