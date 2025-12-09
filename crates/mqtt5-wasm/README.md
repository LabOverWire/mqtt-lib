# mqtt5-wasm

MQTT v5.0 and v3.1.1 WebAssembly client and broker for browser environments.

## Features

- **WebSocket transport** - Connect to remote MQTT brokers
- **In-tab broker** - Run a complete MQTT broker in the browser
- **MessagePort/BroadcastChannel** - Inter-tab communication
- **Broker bridging** - Connect multiple in-browser brokers via MessagePort
- **Full QoS support** - QoS 0, 1, and 2
- **Automatic keepalive** - Connection health monitoring

## Installation

### npm (browser/bundler)

```bash
npm install mqtt5-wasm
```

### Cargo (Rust)

```toml
[dependencies]
mqtt5-wasm = "0.3"
```

Build with wasm-pack:

```bash
wasm-pack build --target web --features client,broker
```

## Usage

```javascript
import init, { WasmMqttClient } from "mqtt5-wasm";

await init();
const client = new WasmMqttClient("browser-client");

await client.connect("ws://broker.example.com:8080/mqtt");

await client.subscribe_with_callback("sensors/#", (topic, payload) => {
  console.log(`${topic}: ${new TextDecoder().decode(payload)}`);
});

await client.publish("sensors/temp", new TextEncoder().encode("25.5"));
```

## Documentation

See the [main repository](https://github.com/LabOverWire/mqtt-lib) for complete documentation and examples.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
