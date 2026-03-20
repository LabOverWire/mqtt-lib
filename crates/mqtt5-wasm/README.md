# mqtt5-wasm

MQTT v5.0 and v3.1.1 WebAssembly client and broker for browser environments.

## Features

- **WebSocket transport** - Connect to remote MQTT brokers via `ws://` or `wss://`
- **In-tab broker** - Run a complete MQTT broker in the browser
- **MessagePort/BroadcastChannel** - Inter-tab and worker communication
- **Broker bridging** - Connect multiple in-browser brokers via MessagePort
- **Full QoS support** - QoS 0, 1, and 2 with proper acknowledgment
- **Shared subscriptions** - Load balancing across multiple subscribers
- **Event callbacks** - Connection, disconnect, error, and connectivity event handlers
- **Browser connectivity detection** - Automatic online/offline detection with smart reconnection
- **Broker lifecycle events** - Monitor client connections, publishes, and subscriptions
- **Automatic keepalive** - Connection health monitoring with timeout detection
- **Will messages** - Last Will and Testament (LWT) support
- **Load balancer redirect** - Server redirect via CONNACK UseAnotherServer for horizontal scaling

## Installation

### npm (browser/bundler)

```bash
npm install mqtt5-wasm
```

### Cargo (Rust)

```toml
[dependencies]
mqtt5-wasm = "1.3"
```

Build with wasm-bindgen:

```bash
cargo build --target wasm32-unknown-unknown --release --features client,broker,codec
wasm-bindgen --target web --out-dir pkg target/wasm32-unknown-unknown/release/mqtt5_wasm.wasm
```

## Usage

### Basic Example

```javascript
import init, { MqttClient } from "mqtt5-wasm";

await init();
const client = new MqttClient("browser-client");

await client.connect("wss://broker.example.com:8084/mqtt");

await client.subscribeWithCallback("sensors/#", (topic, payload) => {
  console.log(`${topic}: ${new TextDecoder().decode(payload)}`);
});

await client.publish("sensors/temp", new TextEncoder().encode("25.5"));

await client.disconnect();
```

### Event Callbacks

```javascript
const client = new MqttClient("browser-client");

client.onConnect((reasonCode, sessionPresent) => {
  console.log(`Connected: reason=${reasonCode}, session=${sessionPresent}`);
});

client.onDisconnect(() => {
  console.log("Disconnected from broker");
});

client.onError((error) => {
  console.error(`Error: ${error}`);
});

await client.connect("wss://broker.example.com:8084/mqtt");
```

### Connectivity Detection

```javascript
const client = new MqttClient("browser-client");

client.onConnectivityChange((online) => {
  console.log(online ? "Network available" : "Network lost");
});

if (!client.isBrowserOnline()) {
  console.log("Currently offline");
}
```

### In-Browser Broker

```javascript
import init, { Broker, MqttClient } from "mqtt5-wasm";

await init();

const broker = new Broker();
const port = broker.createClientPort();

const client = new MqttClient("local-client");
await client.connectMessagePort(port);
```

### Broker Lifecycle Events

Monitor broker activity with lifecycle callbacks:

```javascript
const broker = new Broker();

broker.onClientConnect((event) => {
  console.log(`Client connected: ${event.clientId}, cleanStart: ${event.cleanStart}`);
});

broker.onClientDisconnect((event) => {
  console.log(`Client disconnected: ${event.clientId}, reason: ${event.reason}`);
});

broker.onClientPublish((event) => {
  console.log(`Publish: ${event.topic} (${event.payloadSize} bytes, QoS ${event.qos})`);
});

broker.onClientSubscribe((event) => {
  console.log(`Subscribe: ${event.clientId} -> ${event.subscriptions.map(s => s.topic)}`);
});

broker.onClientUnsubscribe((event) => {
  console.log(`Unsubscribe: ${event.clientId} -> ${event.topics}`);
});

broker.onMessageDelivered((event) => {
  console.log(`Message delivered: packetId=${event.packetId}, QoS ${event.qos}`);
});
```

### Load Balancer Redirect

```javascript
import init, { Broker, BrokerConfig, MqttClient } from "mqtt5-wasm";

await init();

const lbConfig = new BrokerConfig();
lbConfig.allowAnonymous = true;
lbConfig.addLoadBalancerBackend("broker-a");
lbConfig.addLoadBalancerBackend("broker-b");
const lb = Broker.withConfig(lbConfig);

const client = new MqttClient("my-client");
const port = lb.createClientPort();

try {
  await client.connectMessagePort(port);
} catch (err) {
  if (err.type === "redirect") {
    console.log(`Redirected to ${err.url}`);
  }
}
```

## Documentation

See the [main repository](https://github.com/LabOverWire/mqtt-lib) for complete documentation and examples.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
