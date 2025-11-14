//! Example of an MQTT broker with $SYS topics monitoring
//!
//! This example shows how to:
//! - Start an MQTT broker with persistence
//! - Enable $SYS topics for monitoring
//! - Connect a client to view broker statistics

use mqtt5::broker::{BrokerConfig, MqttBroker, StorageBackendType, StorageConfig};
use mqtt5::client::MqttClient;
use mqtt5::types::PublishOptions;
use mqtt5::{Message, QoS};
use mqtt5::time::Duration;
use tokio::time::sleep;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Set up logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    // Configure broker with memory storage
    let storage_config = StorageConfig {
        enable_persistence: true,
        backend: StorageBackendType::Memory,
        base_dir: std::path::PathBuf::from("/tmp/mqtt-broker"),
        cleanup_interval: Duration::from_secs(60),
    };

    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 1883))
        .with_storage(storage_config);

    // Start broker
    let mut broker = MqttBroker::with_config(config).await?;

    // Run broker in background
    let broker_handle = tokio::spawn(async move {
        if let Err(e) = broker.run().await {
            eprintln!("Broker error: {e}");
        }
    });

    // Give broker time to start
    sleep(Duration::from_millis(500)).await;

    // Connect a monitoring client
    let monitor_client = MqttClient::new("monitor-client");
    monitor_client.connect("127.0.0.1:1883").await?;

    // Subscribe to all $SYS topics
    monitor_client
        .subscribe("$SYS/#", |msg: Message| {
            let value = String::from_utf8_lossy(&msg.payload);
            info!("{} = {}", msg.topic, value);
        })
        .await?;
    info!("Subscribed to $SYS topics");

    // Create another client to generate activity
    let test_client = MqttClient::new("test-client");
    test_client.connect("127.0.0.1:1883").await?;

    // Generate some activity
    test_client.publish("test/topic", b"Hello, World!").await?;

    // Publish retained message with QoS and retain flag
    let pub_options = PublishOptions {
        qos: QoS::AtMostOnce,
        retain: true,
        ..Default::default()
    };
    test_client
        .publish_with_options("test/retained", b"Retained message", pub_options)
        .await?;

    // Subscribe to test topics
    test_client
        .subscribe("test/#", |msg: Message| {
            info!(
                "Test client received: {} = {}",
                msg.topic,
                String::from_utf8_lossy(&msg.payload)
            );
        })
        .await?;

    // Wait for $SYS messages
    info!("Monitoring broker statistics for 15 seconds...");
    sleep(Duration::from_secs(15)).await;

    // Disconnect clients
    monitor_client.disconnect().await?;
    test_client.disconnect().await?;

    // Stop broker
    broker_handle.abort();

    info!("Example completed");
    Ok(())
}
