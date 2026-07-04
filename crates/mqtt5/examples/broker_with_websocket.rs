//! Example of running an MQTT broker with WebSocket support
//!
//! This example demonstrates how to configure and run a broker that accepts
//! connections over WebSocket, allowing browser-based MQTT clients to connect.

use mqtt5::broker::config::WebSocketConfig;
use mqtt5::broker::{BrokerConfig, MqttBroker};
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Create broker configuration with WebSocket
    let config = BrokerConfig::default()
        .with_bind_address(([0, 0, 0, 0], 1883))
        .with_websocket(
            WebSocketConfig::new()
                .with_bind_address(([0, 0, 0, 0], 8080))
                .with_path("/mqtt")
                .with_tls(false),
        );

    // Create and run the broker
    let mut broker = MqttBroker::with_config(config).await?;

    info!("MQTT broker started");
    info!("  Plain TCP: mqtt://localhost:1883");
    info!("  WebSocket: ws://localhost:8080/mqtt");
    info!("Press Ctrl+C to stop");

    let shutdown = broker.shutdown_handle();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Shutting down...");
        shutdown.shutdown();
    });

    if let Err(e) = broker.run().await {
        eprintln!("Broker error: {e}");
    }

    Ok(())
}
