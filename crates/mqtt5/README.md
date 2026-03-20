# mqtt5

MQTT v5.0 and v3.1.1 client and broker for native platforms (Linux, macOS, Windows).

## Features

- MQTT v5.0 and v3.1.1 protocol support
- Multiple transports: TCP, TLS, WebSocket, QUIC
- QUIC multistream support with flow headers
- QUIC connection migration for mobile clients
- Automatic reconnection with exponential backoff
- Configurable keepalive with timeout tolerance
- Mock client for unit testing
- OpenTelemetry distributed tracing (optional feature)

## Installation

```toml
[dependencies]
mqtt5 = "0.28"
```

## Quick Start

### Client

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

## Broker

### Core Capabilities

- **MQTT v5.0 and v3.1.1** - Full compliance, cross-version interoperability
- **Multiple QoS levels** - QoS 0, 1, 2 with flow control
- **Session persistence** - Clean start, session expiry, message queuing
- **Retained messages** - Persistent message storage
- **Shared subscriptions** - Load balancing across clients
- **Will messages** - Last Will and Testament (LWT)

### Transport & Security

- **TCP transport** - Standard MQTT over TCP on port 1883
- **TLS/SSL transport** - Secure MQTT over TLS on port 8883
- **WebSocket transport** - MQTT over WebSocket for browsers
- **QUIC transport** - Modern UDP-based transport with built-in TLS 1.3
- **Certificate authentication** - Client certificate validation
- **Username/password authentication** - File-based user management

### Authentication

The broker supports multiple authentication methods:

| Method | Description | Use Case |
|--------|-------------|----------|
| Password | Argon2id hashed passwords | Internal users |
| SCRAM-SHA-256 | Challenge-response, no password transmission | High security |
| JWT | Stateless token verification | Single IdP |
| Federated JWT | Multi-issuer with JWKS auto-refresh | Google, Keycloak, Azure AD |

```rust
use mqtt5::broker::BrokerConfig;
use mqtt5::broker::config::{AuthConfig, AuthMethod};

let config = BrokerConfig::default()
    .with_auth(AuthConfig {
        allow_anonymous: false,
        password_file: Some("passwd.txt".into()),
        auth_method: AuthMethod::Password,
        ..Default::default()
    });
```

Use `CompositeAuthProvider` to chain enhanced auth with a password fallback for internal service clients:

```rust
use mqtt5::broker::auth::{CompositeAuthProvider, PasswordAuthProvider};
use std::sync::Arc;

let primary = broker.auth_provider();
let fallback = Arc::new(PasswordAuthProvider::new());
let broker = broker.with_auth_provider(Arc::new(CompositeAuthProvider::new(primary, fallback)));
```

See [Authentication & Authorization Guide](../../AUTHENTICATION.md) for full details.

### Advanced Features

#### Change-Only Delivery

Reduces bandwidth for topics that frequently publish unchanged values (common with sensors).

```rust
use mqtt5::broker::config::{BrokerConfig, ChangeOnlyDeliveryConfig};

let config = BrokerConfig::new()
    .with_change_only_delivery(ChangeOnlyDeliveryConfig {
        enabled: true,
        topic_patterns: vec!["sensors/#".to_string(), "status/+".to_string()],
    });
```

**How it works:**
- Broker tracks last payload hash per topic per subscriber
- Messages only delivered when payload differs from last delivered value
- State persists across client reconnections
- Configured via topic patterns with wildcard support

**Bridge behavior:**
- Messages from bridges to local subscribers: change-only filtering applies
- Messages to bridges (outgoing): all messages forwarded (no filtering)

#### Load Balancer Mode

Redirects clients to backend brokers using MQTT v5 `UseAnotherServer` (0x9C) with consistent hashing on the client ID.

```bash
mqttv5 broker --host 0.0.0.0:1884 --allow-anonymous
mqttv5 broker --host 0.0.0.0:1885 --allow-anonymous

mqttv5 broker --config lb.json
```

```json
{
  "bind_addresses": ["0.0.0.0:1883"],
  "load_balancer": {
    "backends": ["mqtt://127.0.0.1:1884", "mqtt://127.0.0.1:1885"]
  }
}
```

Clients connecting to port 1883 receive a redirect and automatically reconnect to the assigned backend (up to 3 hops).

```rust
use mqtt5::broker::{MqttBroker, config::{BrokerConfig, LoadBalancerConfig}};

let config = BrokerConfig::new()
    .with_load_balancer(LoadBalancerConfig::new(vec![
        "mqtt://backend1:1883".into(),
        "mqtt://backend2:1883".into(),
    ]));

let broker = MqttBroker::new(config);
broker.start().await?;
```

#### Broker Bridging

Connect two brokers together for message forwarding:

```rust
use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
use mqtt5::QoS;

let bridge_config = BridgeConfig::new("edge-to-cloud", "cloud-broker:1883")
    .add_topic("sensors/+/data", BridgeDirection::Out, QoS::AtLeastOnce)
    .add_topic("commands/+/device", BridgeDirection::In, QoS::AtLeastOnce)
    .add_topic("health/+/status", BridgeDirection::Both, QoS::AtMostOnce);
```

**Bridge Loop Prevention**

When using bidirectional bridges or complex multi-broker topologies, message loops can occur. The broker automatically detects and prevents loops using SHA-256 message fingerprints:

- Each message's fingerprint (hash of topic + payload + QoS + retain) is cached
- Duplicate fingerprints within the TTL window are dropped
- Default TTL: 60 seconds, default cache size: 10,000 fingerprints

```rust
use std::time::Duration;

let mut config = BridgeConfig::new("my-bridge", "remote:1883")
    .add_topic("data/#", BridgeDirection::Both, QoS::AtLeastOnce);

config.loop_prevention_ttl = Duration::from_secs(300);
config.loop_prevention_cache_size = 50000;
```

Or via JSON configuration:

```json
{
  "name": "my-bridge",
  "remote_address": "remote-broker:1883",
  "loop_prevention_ttl": "5m",
  "loop_prevention_cache_size": 50000,
  "topics": [...]
}
```

> **Note:** Legitimate duplicate messages (identical content sent within the TTL window) will also be blocked. Adjust TTL based on your use case.

#### Event Hooks

Monitor and react to broker events with custom handlers:

```rust
use mqtt5::broker::BrokerConfig;
use mqtt5::broker::events::{
    BrokerEventHandler, ClientConnectEvent, ClientPublishEvent, ClientDisconnectEvent,
    PublishAction,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

struct MetricsHandler;

impl BrokerEventHandler for MetricsHandler {
    fn on_client_connect<'a>(
        &'a self,
        event: ClientConnectEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            println!("Client connected: {}", event.client_id);
        })
    }

    fn on_client_publish<'a>(
        &'a self,
        event: ClientPublishEvent,
    ) -> Pin<Box<dyn Future<Output = PublishAction> + Send + 'a>> {
        Box::pin(async move {
            println!("Message published to {}: {} bytes", event.topic, event.payload.len());
            if let Some(response_topic) = &event.response_topic {
                println!("  Response topic: {response_topic}");
            }
            if let Some(correlation_data) = &event.correlation_data {
                println!("  Correlation data: {} bytes", correlation_data.len());
            }
            PublishAction::Continue
        })
    }

    fn on_client_disconnect<'a>(
        &'a self,
        event: ClientDisconnectEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            println!("Client disconnected: {} (reason: {:?})", event.client_id, event.reason);
        })
    }
}

let config = BrokerConfig::default()
    .with_event_handler(Arc::new(MetricsHandler));
```

Available hooks: `on_client_connect`, `on_client_subscribe`, `on_client_unsubscribe`, `on_client_publish`, `on_client_disconnect`, `on_retained_set`, `on_message_delivered`.

The `ClientPublishEvent` includes MQTT 5.0 request/response fields (`response_topic`, `correlation_data`) enabling event handlers to see where clients want responses sent and echo back correlation data.

### Configuration

#### Multi-Transport Broker

```rust
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::broker::config::{TlsConfig, WebSocketConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = BrokerConfig::default()
        .with_bind_address("0.0.0.0:1883".parse()?)
        .with_tls(
            TlsConfig::new("certs/server.crt".into(), "certs/server.key".into())
                .with_ca_file("certs/ca.crt".into())
                .with_bind_address("0.0.0.0:8883".parse()?)
        )
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

## Client

### Core Capabilities

- **MQTT v5.0 and v3.1.1** protocol support
- **Callback-based message handling** with automatic routing
- **Cloud SDK compatible** - Subscribe returns `(packet_id, qos)` tuple
- **Automatic reconnection** with exponential backoff
- **Client-side message queuing** for offline scenarios
- **Reason code validation** for broker publish rejections (ACL, quota limits)

### QUIC Transport

```rust
use mqtt5::MqttClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = MqttClient::new("quic-client");

    client.connect("quic://broker.example.com:14567").await?;
    client.publish("sensors/temp", b"25.5").await?;
    client.disconnect().await?;

    Ok(())
}
```

#### Connection Migration (QUIC)

```rust
use mqtt5::MqttClient;

let client = MqttClient::new("mobile-client");
client.connect("quic://broker.example.com:14567").await?;

client.migrate().await?;
```

See the [QUIC Transport Guide](../../QUIC_IMPLEMENTATION_SPEC.md) for stream strategies, flow headers, and configuration details.

### AWS IoT

```rust
use mqtt5::{MqttClient, ConnectOptions};
use std::time::Duration;

let client = MqttClient::new("aws-iot-device-12345");

client.connect("mqtts://abcdef123456.iot.us-east-1.amazonaws.com:8883").await?;

let (packet_id, qos) = client.subscribe("$aws/things/device-123/shadow/update/accepted", |msg| {
    println!("Shadow update accepted: {:?}", msg.payload);
}).await?;

use mqtt5::validation::namespace::NamespaceValidator;

let validator = NamespaceValidator::aws_iot().with_device_id("device-123");

client.publish("$aws/things/device-123/shadow/update", shadow_data).await?;
```

AWS IoT features:

- AWS IoT endpoint detection
- Topic validation for AWS IoT restrictions and limits
- ALPN protocol support for AWS IoT
- Client certificate loading from bytes (PEM/DER formats)
- SDK compatibility: Subscribe method returns `(packet_id, qos)` tuple

### Testing with Mock Client

```rust
use mqtt5::{MockMqttClient, MqttClientTrait, PublishResult, QoS};

#[tokio::test]
async fn test_my_iot_function() {
    let mock = MockMqttClient::new("test-device");

    mock.set_connect_response(Ok(())).await;
    mock.set_publish_response(Ok(PublishResult::QoS1Or2 { packet_id: 123 })).await;

    my_iot_function(&mock).await.unwrap();

    let calls = mock.get_calls().await;
    assert_eq!(calls.len(), 2);
}

async fn my_iot_function<T: MqttClientTrait>(client: &T) -> Result<(), Box<dyn std::error::Error>> {
    client.connect("mqtt://broker").await?;
    client.publish_qos1("telemetry", b"data").await?;
    Ok(())
}
```

### Keepalive Configuration

```rust
use mqtt5::ConnectOptions;
use mqtt5::types::KeepaliveConfig;
use std::time::Duration;

let options = ConnectOptions::new("client-id")
    .with_keep_alive(Duration::from_secs(30))
    .with_keepalive_config(KeepaliveConfig::new(75, 200));
```

`KeepaliveConfig` controls ping timing and timeout tolerance:
- `ping_interval_percent`: when to send PINGREQ (default 75% of keep_alive)
- `timeout_percent`: how long to wait for PINGRESP (default 150%, use 200%+ for high-latency)

## OpenTelemetry

Distributed tracing with OpenTelemetry support:

```toml
[dependencies]
mqtt5 = { version = "0.27", features = ["opentelemetry"] }
```

- W3C trace context propagation via MQTT user properties
- Span creation for publish/subscribe operations
- Bridge trace context forwarding
- Publisher-to-subscriber observability

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

## Transport URLs

| Transport | URL Format | Port |
|-----------|------------|------|
| TCP | `mqtt://host:port` | 1883 |
| TLS | `mqtts://host:port` | 8883 |
| WebSocket | `ws://host:port/path` | 8080 |
| QUIC | `quic://host:port` | 14567 |

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
