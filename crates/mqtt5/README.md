# mqtt5

MQTT v5.0 and v3.1.1 client and broker for native platforms (Linux, macOS, Windows).

## Features

- MQTT v5.0 and v3.1.1 protocol support
- Multiple transports: TCP, TLS, WebSocket, QUIC
- QUIC multistream support with flow headers
- Automatic reconnection with exponential backoff
- Configurable keepalive with timeout tolerance
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

### Keepalive Configuration

```rust
use mqtt5::{MqttClient, ConnectOptions, KeepaliveConfig};
use std::time::Duration;

let options = ConnectOptions::new("client-id")
    .with_keep_alive(Duration::from_secs(30))
    .with_keepalive_config(KeepaliveConfig::new(75, 200));
```

The `KeepaliveConfig` controls ping timing and timeout tolerance:
- `ping_interval_percent`: When to send PINGREQ (default 75% of keep_alive)
- `timeout_percent`: How long to wait for PINGRESP (default 150%, use 200%+ for high-latency)

### Broker

```rust
use mqtt5::broker::{MqttBroker, BrokerConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut broker = MqttBroker::bind("0.0.0.0:1883").await?;
    broker.run().await?;
    Ok(())
}
```

### Broker with Authentication

```rust
use mqtt5::broker::{BrokerConfig, AuthConfig, AuthMethod};

let config = BrokerConfig::default()
    .with_auth(AuthConfig {
        allow_anonymous: false,
        password_file: Some("passwd.txt".into()),
        auth_method: AuthMethod::Password,
        ..Default::default()
    });
```

Authentication methods: Password, SCRAM-SHA-256, JWT, Federated JWT (Google, Keycloak, etc.)

See [Authentication & Authorization Guide](../../AUTHENTICATION.md) for details.

## Transport URLs

| Transport | URL Format | Port |
|-----------|------------|------|
| TCP | `mqtt://host:port` | 1883 |
| TLS | `mqtts://host:port` | 8883 |
| WebSocket | `ws://host:port/path` | 8080 |
| QUIC | `quic://host:port` | 14567 |

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
