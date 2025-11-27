# mqtt5

MQTT v5.0 client and broker for native platforms (Linux, macOS, Windows).

## Features

- Full MQTT v5.0 protocol compliance
- Multiple transports: TCP, TLS, WebSocket, QUIC
- QUIC multistream support with flow headers
- Automatic reconnection with exponential backoff
- Mock client for unit testing

## Usage

### Client (TCP)

```rust
use mqtt5::{MqttClient, ConnectOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = MqttClient::new("client-id");
    let options = ConnectOptions::new("client-id".to_string());

    client.connect_with_options("mqtt://localhost:1883", options).await?;
    client.publish("topic", b"message").await?;
    client.disconnect().await?;

    Ok(())
}
```

### Client (QUIC)

```rust
use mqtt5::{MqttClient, ConnectOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = MqttClient::new("quic-client");

    // QUIC transport with built-in TLS 1.3
    client.connect("quic://broker.example.com:14567").await?;
    client.publish("sensors/temp", b"25.5").await?;
    client.disconnect().await?;

    Ok(())
}
```

### Broker

```rust
use mqtt5::broker::{MqttBroker, BrokerConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = BrokerConfig::builder()
        .with_tcp_bind("127.0.0.1:1883")?
        .build()?;

    let broker = MqttBroker::new(config);
    broker.start().await?;

    Ok(())
}
```

## Transport URLs

| Transport | URL Format | Port |
|-----------|------------|------|
| TCP | `mqtt://host:port` | 1883 |
| TLS | `mqtts://host:port` | 8883 |
| WebSocket | `ws://host:port/path` | 8080 |
| QUIC | `quic://host:port` | 14567 |

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
