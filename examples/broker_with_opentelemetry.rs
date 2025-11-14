//! Example of an MQTT broker with OpenTelemetry tracing
//!
//! This example demonstrates:
//! - Initializing OpenTelemetry with OTLP exporter
//! - Running an MQTT broker with distributed tracing
//! - Client publishing messages with trace context propagation
//!
//! To run this example:
//! 1. Start a local OTLP collector (e.g., Jaeger):
//!    ```bash
//!    docker run -d --name jaeger \
//!      -e COLLECTOR_OTLP_ENABLED=true \
//!      -p 16686:16686 \
//!      -p 4317:4317 \
//!      -p 4318:4318 \
//!      jaegertracing/all-in-one:latest
//!    ```
//!
//! 2. Run the example with the opentelemetry feature:
//!    ```bash
//!    cargo run --example broker_with_opentelemetry --features opentelemetry
//!    ```
//!
//! 3. Open Jaeger UI at http://localhost:16686 to view traces

use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::telemetry::TelemetryConfig;
use mqtt5::MqttClient;
use mqtt5::time::Duration;
use tokio::time::sleep;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let telemetry_config = TelemetryConfig::new("mqtt-broker-example")
        .with_endpoint("http://localhost:4317")
        .with_sampling_ratio(1.0)
        .with_timeout(Duration::from_secs(10));

    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 1883))
        .with_opentelemetry(telemetry_config);

    let mut broker = MqttBroker::with_config(config).await?;
    info!("MQTT broker started on 127.0.0.1:1883 with OpenTelemetry enabled");

    let broker_handle = tokio::spawn(async move {
        if let Err(e) = broker.run().await {
            eprintln!("Broker error: {e}");
        }
    });

    sleep(Duration::from_millis(500)).await;

    let client = MqttClient::new("otel-demo-client");
    client.connect("127.0.0.1:1883").await?;
    info!("Client connected");

    client
        .subscribe("test/#", |msg| {
            info!(
                "Received message on {}: {}",
                msg.topic,
                String::from_utf8_lossy(&msg.payload)
            );
        })
        .await?;
    info!("Subscribed to test/#");

    for i in 0..5 {
        let message = format!("Message {i} with trace context");
        client.publish("test/telemetry", message.as_bytes()).await?;
        info!("Published message {i}");
        sleep(Duration::from_millis(500)).await;
    }

    info!("Traces have been exported to the OTLP collector");
    info!("View traces at http://localhost:16686 (Jaeger UI)");

    sleep(Duration::from_secs(2)).await;

    client.disconnect().await?;
    broker_handle.abort();

    info!("Example completed");
    Ok(())
}
