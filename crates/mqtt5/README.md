# mqtt5

MQTT v5.0 client and broker for native platforms (Linux, macOS, Windows).

## Usage

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
    let config = BrokerConfig::builder()
        .with_tcp_bind("127.0.0.1:1883")?
        .build()?;

    let broker = MqttBroker::new(config);
    broker.start().await?;

    Ok(())
}
```

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
