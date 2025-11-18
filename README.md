# Complete MQTT v5.0 Platform

[![Crates.io](https://img.shields.io/crates/v/mqtt5.svg)](https://crates.io/crates/mqtt5)
[![Documentation](https://docs.rs/mqtt5/badge.svg)](https://docs.rs/mqtt5)
[![Rust CI](https://github.com/fabriciobracht/mqtt-lib/workflows/Rust%20CI/badge.svg)](https://github.com/fabriciobracht/mqtt-lib/actions)
[![License](https://img.shields.io/crates/l/mqtt5.svg)](https://github.com/fabriciobracht/mqtt-lib#license)

**MQTT v5.0 platform featuring client library and broker implementation**

## Dual Architecture: Client + Broker

| Component       | Use Case                         | Key Features                                         |
| --------------- | -------------------------------- | ---------------------------------------------------- |
| **MQTT Broker** | Run your own MQTT infrastructure | TLS, WebSocket, Authentication, Bridging, Monitoring |
| **MQTT Client** | Connect to any MQTT broker       | Cloud compatible, Auto-reconnect, Mock testing       |

## 📦 Installation

### Library Crate

```toml
[dependencies]
mqtt5 = "0.9.0"
```

### CLI Tool

```bash
cargo install mqttv5-cli
```

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

    // Connect to your broker (supports multiple transports)
    client.connect("mqtt://localhost:1883").await?;       // TCP
    // client.connect("mqtts://localhost:8883").await?;   // TLS
    // client.connect("ws://localhost:8080/mqtt").await?; // WebSocket

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
git clone https://github.com/fabriciobracht/mqtt-lib
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

# Smart prompting when arguments are missing
mqttv5 pub
# ? MQTT topic › sensors/
# ? Message content › Hello World!
# ? Quality of Service level › ● 0 (At most once)
```

### CLI Features

- Unified interface - Single binary with broker, pub, sub, passwd, and acl commands
- Smart prompting - Guides users when arguments are missing
- Input validation - Catches errors with helpful suggestions
- Descriptive flags - `--topic` instead of `-t`, with short aliases available
- Interactive & non-interactive - Works for both humans and scripts

## Platform Features

### Broker

- Multiple transports: TCP, TLS, WebSocket in a single binary
- Built-in authentication: Username/password, file-based, bcrypt
- Resource monitoring: Connection limits, rate limiting, memory tracking
- Distributed tracing: OpenTelemetry integration with trace context propagation
- Self-contained: No external dependencies

### Client

- Cloud compatible: Works with cloud MQTT brokers
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
- Reason code validation - Properly handles broker publish rejections (ACL, quota limits)

### Transport & Connectivity

- Certificate loading from bytes - Load TLS certificates from memory (PEM/DER formats)
- WebSocket transport - MQTT over WebSocket for browsers
- TLS/SSL support - Secure connections with certificate validation
- Session persistence - Survives disconnections with clean_start=false

### Testing & Development

- Mockable Client Interface - `MqttClientTrait` enables testing without real brokers
- Property-based testing - 29 tests ensuring robustness
- CLI Integration Testing - End-to-end tests with real broker verification
- Flow control - Respects broker receive maximum limits

## WASM Browser Support

The library compiles to WebAssembly for browser environments with two deployment modes:

### External Broker Mode
Connect to remote MQTT brokers using WebSocket transport:

```javascript
import init, { WasmMqttClient } from './pkg/mqtt5.js';

await init();
const client = new WasmMqttClient('browser-client');
await client.connect('ws://broker.example.com:8080/mqtt');

await client.subscribe_with_callback('sensors/temp', (topic, payload) => {
  const decoder = new TextDecoder();
  console.log('Temperature:', decoder.decode(payload));
});
```

### In-Tab Broker Mode
Run a complete MQTT broker inside your browser tab:

```javascript
import init, { WasmBroker, WasmMqttClient } from './pkg/mqtt5.js';

await init();
const broker = new WasmBroker();
const client = new WasmMqttClient('local-client');

const port = broker.create_client_port();
await client.connect_message_port(port);
```

The in-tab broker supports all core MQTT v5.0 features including QoS levels, retained messages, and subscriptions. Perfect for testing, demos, and offline-capable applications.

**WASM Limitations:** No TLS socket support (use browser-managed `wss://`), memory-only storage, no file I/O. See `examples/wasm/README.md` for details.

**Examples:** See `examples/wasm/` for complete browser examples.

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

- Endpoint detection: Detects AWS IoT endpoints
- Topic validation: Built-in validation for AWS IoT topic restrictions and limits
- ALPN support: TLS configuration with AWS IoT ALPN protocol
- Certificate loading: Load client certificates from bytes (PEM/DER formats)
- SDK compatibility: Subscribe method returns `(packet_id, qos)` tuple

## OpenTelemetry Integration

Enable distributed tracing across your MQTT infrastructure with OpenTelemetry support:

```toml
[dependencies]
mqtt5 = { version = "0.9.0", features = ["opentelemetry"] }
```

### Features

- W3C trace context propagation via MQTT user properties
- Automatic span creation for publish/subscribe operations
- Bridge trace context forwarding
- Complete observability from publisher to subscriber

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

See `examples/broker_with_opentelemetry.rs` for a complete example.

## Development & Building

### Prerequisites

- Rust 1.83 or later
- cargo-make (`cargo install cargo-make`)

### Quick Setup

```bash
# Clone the repository
git clone https://github.com/fabriciobracht/mqtt-lib.git
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
cargo run --example simple_client           # Basic client usage
cargo run --example simple_broker           # Start basic broker
cargo run --example broker_with_tls         # TLS-enabled broker
cargo run --example broker_with_websocket   # WebSocket-enabled broker
cargo run --example broker_all_transports   # Broker with all transports (TCP/TLS/WebSocket)
cargo run --example broker_bridge_demo      # Broker bridging demo
cargo run --example broker_with_monitoring  # Broker with $SYS topics
cargo run --example broker_with_opentelemetry --features opentelemetry  # Distributed tracing
cargo run --example shared_subscription_demo # Shared subscription load balancing
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
- Username/password authentication with bcrypt hashing
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
