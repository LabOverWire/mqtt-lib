# MQTT v5.0 Platform Examples

This directory contains examples demonstrating various features of the MQTT v5.0 platform, including both client and broker functionality.

## Getting Started

### Basic Examples

Start with these examples:

1. **simple_broker.rs** - Basic MQTT broker
   ```bash
   cargo run -p mqtt5 --example simple_broker
   ```
   Starts a broker on localhost:1883 that accepts anonymous connections.

2. **simple_client.rs** - Basic MQTT client with embedded broker
   ```bash
   cargo run -p mqtt5 --example simple_client
   ```
   Demonstrates pub/sub with an embedded broker (or external if MQTT_BROKER env is set).

## Client Examples

- **simple_client.rs** - Basic publish/subscribe operations
- **shared_subscription_demo.rs** - Load balancing with shared subscriptions

## Broker Examples

### Basic Broker Setup

- **simple_broker.rs** - Minimal broker setup
- **broker_with_tls.rs** - Secure broker with TLS
- **broker_with_websocket.rs** - WebSocket support
- **broker_all_transports.rs** - TCP, TLS, and WebSocket together

### Advanced Broker Features

- **broker_with_monitoring.rs** - Metrics and monitoring
- **broker_bridge_demo.rs** - Broker-to-broker bridging
- **broker_with_opentelemetry.rs** - Distributed tracing with OpenTelemetry (requires `--features opentelemetry`)

## Running Examples

### Prerequisites

1. Install Rust (1.70+)
2. Clone the repository
3. Build the project:
   ```bash
   cargo make build
   ```

### Basic Usage

Run any example with:
```bash
cargo run -p mqtt5 --example <example_name>

# For examples requiring features
cargo run -p mqtt5 --example broker_with_opentelemetry --features opentelemetry
```

### Environment Variables

Some examples support configuration via environment variables:

- `MQTT_BROKER` - External broker URL (default: uses embedded broker)
- `RUST_LOG` - Logging level (e.g., `mqtt_v5=debug`)

### Testing Client-Broker Communication

1. Start a broker in one terminal:
   ```bash
   cargo run --example simple_broker
   ```

2. Run a client in another terminal:
   ```bash
   cargo run --example simple_client
   ```

### Using External Tools

Test with MQTT tools:

```bash
# Using mqttv5 CLI
# Subscribe to all topics
mqttv5 sub --host localhost --topic '#' --verbose

# Publish a message
mqttv5 pub --host localhost --topic 'test/topic' --message 'Hello!'

# Or with mosquitto tools
mosquitto_sub -h localhost -t '#' -v
mosquitto_pub -h localhost -t 'test/topic' -m 'Hello!'

# Monitor with MQTT Explorer
# Download from: http://mqtt-explorer.com/
```

## Example Descriptions

### Client Examples

#### simple_client.rs
- Basic connection and pub/sub
- QoS levels demonstration
- Retained messages
- JSON payload handling

#### shared_subscription_demo.rs
- Load balancing across workers
- Shared subscription groups
- Work distribution patterns

### Broker Examples

#### simple_broker.rs
- Minimal broker setup
- Anonymous connections
- Basic configuration

#### broker_with_tls.rs
- TLS encryption
- Certificate configuration
- Secure connections

#### broker_with_websocket.rs
- WebSocket transport
- Browser client support
- HTTP upgrade handling

#### broker_all_transports.rs
- Multiple transports simultaneously
- TCP, TLS, and WebSocket
- Port configuration for all transport types

#### broker_with_monitoring.rs
- $SYS topics
- Client statistics
- Message counters
- Performance metrics

#### broker_bridge_demo.rs
- Connect multiple brokers
- Topic mapping
- Message routing
- Bridge configuration

#### broker_with_opentelemetry.rs
- OpenTelemetry distributed tracing
- W3C trace context propagation
- OTLP exporter configuration
- End-to-end trace visibility

## Common Patterns

### Error Handling

All examples use proper error handling:
```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Example code
    Ok(())
}
```

### Logging

Examples use the `tracing` crate:
```rust
// Enable debug logging
RUST_LOG=mqtt_v5=debug cargo run --example simple_client
```

### Graceful Shutdown

Examples handle Ctrl+C properly:
```rust
tokio::signal::ctrl_c().await?;
println!("Shutting down...");
```

## Next Steps

1. Start with `simple_broker` and `simple_client`
2. Try specific examples based on your use case
3. Modify examples for your own applications

## Contributing

Feel free to submit new examples demonstrating:
- Specific use cases
- Integration patterns
- Performance optimizations
- Security configurations

## License

All examples are part of the mqtt-v5 project and follow the same license.