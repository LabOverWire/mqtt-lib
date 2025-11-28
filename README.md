# Complete MQTT v5.0 Platform

[![Crates.io](https://img.shields.io/crates/v/mqtt5.svg)](https://crates.io/crates/mqtt5)
[![Documentation](https://docs.rs/mqtt5/badge.svg)](https://docs.rs/mqtt5)
[![Rust CI](https://github.com/LabOverWire/mqtt-lib/actions/workflows/rust.yml/badge.svg)](https://github.com/LabOverWire/mqtt-lib/actions)
[![License](https://img.shields.io/crates/l/mqtt5.svg)](https://github.com/LabOverWire/mqtt-lib#license)

**MQTT v5.0 platform featuring client library and broker implementation**

## Dual Architecture: Client + Broker

| Component       | Use Case                         | Key Features                                              |
| --------------- | -------------------------------- | --------------------------------------------------------- |
| **MQTT Broker** | Run your own MQTT infrastructure | TLS, WebSocket, QUIC, Authentication, Bridging, Monitoring |
| **MQTT Client** | Connect to any MQTT broker       | Cloud compatible, QUIC multistream, Auto-reconnect, Mock testing |

## 📦 Installation

### Library Crate

```toml
[dependencies]
mqtt5 = "0.11.1"
```

### CLI Tool

```bash
cargo install mqttv5-cli
```

## Crate Organization

The platform is organized into three crates:

- **mqtt5-protocol** - Platform-agnostic MQTT v5.0 core (packets, types, Transport trait)
- **mqtt5** - Native client and broker for Linux, macOS, Windows
- **mqtt5-wasm** - WebAssembly client and broker for browsers

Shared protocol implementation from `mqtt5-protocol`.

## Quick Start

### Start an MQTT Broker

```rust
use mqtt5::broker::{BrokerConfig, MqttBroker};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create broker with default configuration
    let mut broker = MqttBroker::bind("0.0.0.0:1883").await?;

    println!("MQTT broker running on port 1883");

    // Run until shutdown
    broker.run().await?;
    Ok(())
}
```

### Connect a Client

```rust
use mqtt5::{MqttClient, QoS};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = MqttClient::new("my-device-001");

    // Multiple transport options:
    client.connect("mqtt://localhost:1883").await?;        // TCP
    // client.connect("mqtts://localhost:8883").await?;    // TLS
    // client.connect("ws://localhost:8080/mqtt").await?;  // WebSocket
    // client.connect("quic://localhost:14567").await?;    // QUIC

    // Subscribe with callback
    client.subscribe("sensors/+/data", |msg| {
        println!("{}: {}", msg.topic, String::from_utf8_lossy(&msg.payload));
    }).await?;

    // Publish a message
    client.publish("sensors/temp/data", b"25.5°C").await?;

    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    Ok(())
}
```

## Command Line Interface (mqttv5)

### Installation

```bash
# Install from crates.io
cargo install mqttv5-cli

# Or build from source
git clone https://github.com/LabOverWire/mqtt-lib
cd mqtt-lib
cargo build --release -p mqttv5-cli
```

### Usage Examples

```bash
# Start a broker
mqttv5 broker --host 0.0.0.0:1883

# Publish a message
mqttv5 pub --topic "sensors/temperature" --message "23.5"

# Subscribe to topics
mqttv5 sub --topic "sensors/+" --verbose

# Use different transports with --url flag
mqttv5 pub --url mqtt://localhost:1883 --topic test --message "TCP"
mqttv5 pub --url mqtts://localhost:8883 --topic test --message "TLS"
mqttv5 pub --url ws://localhost:8080/mqtt --topic test --message "WebSocket"
mqttv5 pub --url quic://localhost:14567 --topic test --message "QUIC"

# Smart prompting when arguments are missing
mqttv5 pub
# ? MQTT topic › sensors/
# ? Message content › Hello World!
# ? Quality of Service level › ● 0 (At most once)
```

### CLI Features

- Single binary: broker, pub, sub, passwd, acl commands
- Interactive prompts for missing arguments
- Input validation with error messages
- Long flags (`--topic`) with short aliases (`-t`)
- Interactive and non-interactive modes

## Platform Features

### Broker

- Multiple transports: TCP, TLS, WebSocket, QUIC in a single binary
- Built-in authentication: Username/password, file-based, argon2
- Resource monitoring: Connection limits, rate limiting, memory tracking
- Distributed tracing: OpenTelemetry integration with trace context propagation
- Self-contained: No external dependencies

### Client

- Cloud MQTT broker support (AWS IoT, Azure IoT Hub, etc.)
- Automatic reconnection with exponential backoff
- Direct async/await patterns
- Comprehensive testing support

## Broker Capabilities

### Core MQTT v5.0

- Full MQTT v5.0 compliance - All packet types, properties, reason codes
- Multiple QoS levels - QoS 0, 1, 2 with flow control
- Session persistence - Clean start, session expiry, message queuing
- Retained messages - Persistent message storage
- Shared subscriptions - Load balancing across clients
- Will messages - Last Will and Testament (LWT)

### Transport & Security

- TCP transport - Standard MQTT over TCP on port 1883
- TLS/SSL transport - Secure MQTT over TLS on port 8883
- WebSocket transport - MQTT over WebSocket for browsers
- QUIC transport - Modern UDP-based transport with built-in TLS 1.3
- Certificate authentication - Client certificate validation
- Username/password authentication - File-based user management

### Advanced Features

- Broker-to-broker bridging - Connect multiple broker instances
- Resource monitoring - $SYS topics, connection metrics
- Hot configuration reload - Update settings without restart
- Storage backends - File-based or in-memory persistence

## Client Capabilities

### Core MQTT v5.0

- Full MQTT v5.0 protocol compliance
- Callback-based message handling with automatic routing
- Cloud SDK compatible - Subscribe returns `(packet_id, qos)` tuple
- Automatic reconnection with exponential backoff
- Client-side message queuing for offline scenarios
- Reason code validation for broker publish rejections (ACL, quota limits)

### Transport & Connectivity

- Certificate loading from memory (PEM/DER formats)
- WebSocket transport - MQTT over WebSocket for browsers
- TLS/SSL support - Secure connections with certificate validation
- QUIC transport - UDP-based with multistream support and flow headers
- Session persistence - Survives disconnections with clean_start=false

### Testing & Development

- Mockable Client Interface - `MqttClientTrait` for unit testing
- Property-based testing - 29 tests with Proptest
- CLI Integration Testing - End-to-end tests
- Flow control - Broker receive maximum limits

## QUIC Transport

MQTT over QUIC provides modern, high-performance transport with built-in encryption.

### Features

- **Built-in TLS 1.3** - QUIC mandates encryption, no separate TLS handshake
- **Multistream support** - Parallel MQTT operations without head-of-line blocking
- **0-RTT connection** - Reduced latency for resumed connections
- **Connection migration** - Seamless IP address changes (mobile networks)
- **Flow headers** - Stream state recovery for persistent QoS sessions

### Client Usage

```rust
use mqtt5::{MqttClient, ConnectOptions};
use mqtt5::transport::quic::QuicClientConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = MqttClient::new("quic-client");

    // Basic QUIC connection (insecure, for testing)
    client.connect("quic://broker.example.com:14567").await?;

    // With certificate verification
    let quic_config = QuicClientConfig::builder()
        .with_ca_cert_file("ca.crt")?
        .with_server_name("broker.example.com")
        .build()?;

    let options = ConnectOptions::new("quic-client".to_string())
        .with_quic_config(quic_config);

    client.connect_with_options("quic://broker.example.com:14567", options).await?;

    Ok(())
}
```

### Stream Strategies

QUIC multistream support allows different stream allocation strategies:

| Strategy | Description | Use Case |
|----------|-------------|----------|
| `ControlOnly` | Single stream for all packets | Simple deployments |
| `DataPerPublish` | New stream per QoS 1/2 publish | High-throughput publishing |
| `DataPerTopic` | Dedicated stream per topic | Topic isolation |
| `DataPerSubscription` | Stream per subscription | Subscriber isolation |

### Flow Headers

Flow headers enable session state recovery across stream failures:

- **Flow ID** - Unique identifier for stream state tracking
- **Expire interval** - Time before server discards flow state
- **Flags** - Recovery mode, persistent QoS, subscription state

### Compatibility

Compatible with MQTT-over-QUIC brokers:
- EMQX 5.0+ (native QUIC support)
- Other QUIC-enabled MQTT brokers

## WASM Browser Support

WebAssembly builds for browser environments with three deployment modes.

### Installation

```bash
npm install mqtt5-wasm
```

### Connection Modes

#### External Broker Mode (WebSocket)

Connect to remote MQTT brokers using WebSocket transport:

```javascript
import init, { WasmMqttClient } from "mqtt5-wasm";

await init();
const client = new WasmMqttClient("browser-client");

await client.connect("ws://broker.example.com:8080/mqtt");
```

#### In-Tab Broker Mode (MessagePort)

MQTT broker in a browser tab:

```javascript
import init, { WasmBroker, WasmMqttClient } from "mqtt5-wasm";

await init();
const broker = new WasmBroker();
const client = new WasmMqttClient("local-client");

const port = broker.create_client_port();
await client.connect_message_port(port);
```

#### Cross-Tab Mode (BroadcastChannel)

Communication across browser tabs via BroadcastChannel API:

```javascript
await client.connect_broadcast_channel("mqtt-channel");
```

### Complete API Reference

#### Publishing Messages

**QoS 0 (Fire-and-forget):**

```javascript
const encoder = new TextEncoder();
await client.publish("sensors/temp", encoder.encode("25.5°C"));
```

**QoS 1 (At least once):**

```javascript
await client.publish_qos1("sensors/temp", encoder.encode("25.5°C"), (reasonCode) => {
  if (reasonCode === 0) {
    console.log("Message acknowledged");
  } else {
    console.error("Publish failed, reason:", reasonCode);
  }
});
```

**QoS 2 (Exactly once):**

```javascript
await client.publish_qos2("commands/action", encoder.encode("start"), (result) => {
  if (typeof result === "number") {
    console.log("Success, reason code:", result);
  } else {
    console.error("Timeout or error:", result);
  }
});
```

#### Subscribing to Topics

**With callback (recommended):**

```javascript
await client.subscribe_with_callback("sensors/+/data", (topic, payload) => {
  const decoder = new TextDecoder();
  console.log(`${topic}: ${decoder.decode(payload)}`);
});
```

**Without callback:**

```javascript
const packetId = await client.subscribe("sensors/#");
console.log("Subscribed with packet ID:", packetId);
```

**Unsubscribe:**

```javascript
await client.unsubscribe("sensors/temp");
```

#### Connection Events

**Connection success:**

```javascript
client.on_connect((reasonCode, sessionPresent) => {
  console.log("Connected!");
  console.log("Reason code:", reasonCode);
  console.log("Session present:", sessionPresent);
});
```

**Disconnection:**

```javascript
client.on_disconnect(() => {
  console.log("Disconnected from broker");
});
```

**Errors (including keepalive timeout):**

```javascript
client.on_error((error) => {
  console.error("Error:", error);
});
```

**Check connection status:**

```javascript
if (client.is_connected()) {
  console.log("Currently connected");
}
```

**Manual disconnect:**

```javascript
await client.disconnect();
```

### Automatic Features

#### Keepalive & Timeout Detection

- Sends PINGREQ every 30 seconds
- Connection timeout after 90 seconds
- Triggers `on_error("Keepalive timeout")` and `on_disconnect()` on timeout

#### QoS 2 Flow Management

- Full four-way handshake (PUBLISH → PUBREC → PUBREL → PUBCOMP)
- 10-second timeout for incomplete flows
- Duplicate detection with 30-second tracking window
- Status updates via callback

### In-Tab Broker Features

MQTT v5.0 broker in browser:

- QoS levels: 0, 1, 2
- Retained messages: in-memory storage
- Subscriptions: wildcard matching (`+`, `#`)
- Session management: memory-only (lost on page reload)
- No external dependencies

### WASM Limitations

- Browser-managed `wss://` only
- No file I/O (IndexedDB/localStorage available for persistence)
- WebSocket/MessagePort/BroadcastChannel only
- JavaScript event loop execution

### Browser Compatibility

- Chrome/Edge 90+
- Firefox 88+
- Safari 15.4+

### Complete Examples

See `crates/mqtt5-wasm/examples/` for browser examples:

- `websocket/` - External broker connections
- `local-broker/` - In-tab broker with MessagePort
- Complete HTML/JavaScript/CSS applications
- Build instructions with `wasm-pack`

## Advanced Broker Configuration

### Multi-Transport Broker

```rust
use mqtt5::broker::{BrokerConfig, TlsConfig, WebSocketConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = BrokerConfig::default()
        // TCP on port 1883
        .with_bind_address("0.0.0.0:1883".parse()?)
        // TLS on port 8883
        .with_tls(
            TlsConfig::new("certs/server.crt".into(), "certs/server.key".into())
                .with_ca_file("certs/ca.crt".into())
                .with_bind_address("0.0.0.0:8883".parse()?)
        )
        // WebSocket on port 8080
        .with_websocket(
            WebSocketConfig::default()
                .with_bind_address("0.0.0.0:8080".parse()?)
                .with_path("/mqtt")
        )
        .with_max_clients(10_000);

    let mut broker = MqttBroker::with_config(config).await?;

    println!("Multi-transport MQTT broker running:");
    println!("  TCP:       mqtt://localhost:1883");
    println!("  TLS:       mqtts://localhost:8883");
    println!("  WebSocket: ws://localhost:8080/mqtt");
    println!("  QUIC:      quic://localhost:14567");

    broker.run().await?;
    Ok(())
}
```

### Broker with Authentication

```rust
use mqtt5::broker::{BrokerConfig, AuthConfig, AuthMethod};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let auth_config = AuthConfig {
        allow_anonymous: false,
        password_file: Some("users.txt".into()),
        auth_method: AuthMethod::Password,
        auth_data: None,
    };

    let config = BrokerConfig::default()
        .with_bind_address("0.0.0.0:1883".parse()?)
        .with_auth(auth_config);

    let mut broker = MqttBroker::with_config(config).await?;
    broker.run().await?;
    Ok(())
}
```

### Broker Bridging

```rust
use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
use mqtt5::QoS;

// Connect two brokers together
let bridge_config = BridgeConfig::new("edge-to-cloud", "cloud-broker:1883")
    // Forward sensor data from edge to cloud
    .add_topic("sensors/+/data", BridgeDirection::Out, QoS::AtLeastOnce)
    // Receive commands from cloud to edge
    .add_topic("commands/+/device", BridgeDirection::In, QoS::AtLeastOnce)
    // Bidirectional health monitoring
    .add_topic("health/+/status", BridgeDirection::Both, QoS::AtMostOnce);

// Add bridge to broker (broker handles connection management)
// broker.add_bridge(bridge_config).await?;
```

## Testing Support

### Unit Testing with Mock Client

```rust
use mqtt5::{MockMqttClient, MqttClientTrait, PublishResult, QoS};

#[tokio::test]
async fn test_my_iot_function() {
    // Create mock client
    let mock = MockMqttClient::new("test-device");

    // Configure mock responses
    mock.set_connect_response(Ok(())).await;
    mock.set_publish_response(Ok(PublishResult::QoS1Or2 { packet_id: 123 })).await;

    // Test your function that accepts MqttClientTrait
    my_iot_function(&mock).await.unwrap();

    // Verify the calls
    let calls = mock.get_calls().await;
    assert_eq!(calls.len(), 2); // connect + publish
}

// Your production code uses the trait
async fn my_iot_function<T: MqttClientTrait>(client: &T) -> Result<(), Box<dyn std::error::Error>> {
    client.connect("mqtt://broker").await?;
    client.publish_qos1("telemetry", b"data").await?;
    Ok(())
}
```

## AWS IoT Support

The client library includes AWS IoT compatibility features:

```rust
use mqtt5::{MqttClient, ConnectOptions};
use std::time::Duration;

let client = MqttClient::new("aws-iot-device-12345");

// Connect to AWS IoT endpoint
client.connect("mqtts://abcdef123456.iot.us-east-1.amazonaws.com:8883").await?;

// Subscribe returns (packet_id, qos) tuple for compatibility
let (packet_id, qos) = client.subscribe("$aws/things/device-123/shadow/update/accepted", |msg| {
    println!("Shadow update accepted: {:?}", msg.payload);
}).await?;

// AWS IoT topic validation prevents publishing to reserved topics
use mqtt5::validation::namespace::NamespaceValidator;

let validator = NamespaceValidator::aws_iot().with_device_id("device-123");

// This will succeed - device can update its own shadow
client.publish("$aws/things/device-123/shadow/update", shadow_data).await?;

// This will be rejected - device cannot publish to shadow response topics
// client.publish("$aws/things/device-123/shadow/update/accepted", data).await?; // Error!
```

AWS IoT features:

- AWS IoT endpoint detection
- Topic validation for AWS IoT restrictions and limits
- ALPN protocol support for AWS IoT
- Client certificate loading from bytes (PEM/DER formats)
- SDK compatibility: Subscribe method returns `(packet_id, qos)` tuple

## OpenTelemetry Integration

Distributed tracing with OpenTelemetry support:

```toml
[dependencies]
mqtt5 = { version = "0.11.0", features = ["opentelemetry"] }
```

### Features

- W3C trace context propagation via MQTT user properties
- Span creation for publish/subscribe operations
- Bridge trace context forwarding
- Publisher-to-subscriber observability

### Example

```rust
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::telemetry::TelemetryConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let telemetry_config = TelemetryConfig::new("mqtt-broker")
        .with_endpoint("http://localhost:4317")
        .with_sampling_ratio(1.0);

    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 1883))
        .with_opentelemetry(telemetry_config);

    let mut broker = MqttBroker::with_config(config).await?;
    broker.run().await?;
    Ok(())
}
```

See `crates/mqtt5/examples/broker_with_opentelemetry.rs` for a complete example.

## Development & Building

### Prerequisites

- Rust 1.83 or later
- cargo-make (`cargo install cargo-make`)

### Quick Setup

```bash
# Clone the repository
git clone https://github.com/LabOverWire/mqtt-lib.git
cd mqtt-lib

# Install development tools and git hooks
./scripts/install-hooks.sh

# Run all CI checks locally
cargo make ci-verify
```

### Available Commands

```bash
# Development
cargo make build          # Build the project
cargo make build-release  # Build optimized release version
cargo make test           # Run all tests
cargo make fmt            # Format code
cargo make clippy         # Run linter

# CI/CD
cargo make ci-verify      # Run ALL CI checks (must pass before push)
cargo make pre-commit     # Run before committing (fmt + clippy + test)

# Examples (use raw cargo for specific targets)
cargo run -p mqtt5 --example simple_client           # Basic client usage
cargo run -p mqtt5 --example simple_broker           # Start basic broker
cargo run -p mqtt5 --example broker_with_tls         # TLS-enabled broker
cargo run -p mqtt5 --example broker_with_websocket   # WebSocket-enabled broker
cargo run -p mqtt5 --example broker_all_transports   # Broker with all transports (TCP/TLS/WebSocket)
cargo run -p mqtt5 --example broker_bridge_demo      # Broker bridging demo
cargo run -p mqtt5 --example broker_with_monitoring  # Broker with $SYS topics
cargo run -p mqtt5 --example broker_with_opentelemetry --features opentelemetry  # Distributed tracing
cargo run -p mqtt5 --example shared_subscription_demo # Shared subscription load balancing
```

### Testing

```bash
# Generate test certificates (required for TLS tests)
./scripts/generate_test_certs.sh

# Run unit tests (fast)
cargo make test-fast

# Run all tests including integration tests
cargo make test
```

## Architecture

This project follows Rust async patterns:

- Direct async methods for all operations
- Shared state via `Arc<RwLock<T>>`
- Connection pooling and buffer reuse

## Security

- TLS 1.2+ support with certificate validation
- Username/password authentication with argon2 hashing
- Rate limiting
- Resource monitoring
- Client certificate authentication for mutual TLS
- Access Control Lists (ACL) with `mqttv5 acl` CLI management (add, remove, list, check)

## License

This project is licensed under either of

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## Documentation

- [Architecture Overview](ARCHITECTURE.md) - System design and principles
- [CLI Usage Guide](crates/mqttv5-cli/CLI_USAGE.md) - Complete CLI reference and examples
- [API Documentation](https://docs.rs/mqtt5) - API reference
