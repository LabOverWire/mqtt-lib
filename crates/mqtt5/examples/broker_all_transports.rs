//! Example of running an MQTT broker with all transport types
//!
//! This example demonstrates how to configure and run a broker that accepts:
//! - Plain TCP connections (port 1883)
//! - TLS connections (port 8883)
//! - WebSocket connections (port 8080)

use mqtt5::broker::config::{TlsConfig, WebSocketConfig};
use mqtt5::broker::{BrokerConfig, MqttBroker};
use std::path::PathBuf;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize crypto provider for TLS
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install crypto provider");

    // Initialize logging
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .try_init();

    // Create broker configuration with all transport types
    let config = BrokerConfig::default()
        .with_bind_address(([0, 0, 0, 0], 1883))
        .with_tls(
            TlsConfig::new(
                PathBuf::from("test_certs/server.pem"),
                PathBuf::from("test_certs/server.key"),
            )
            .with_ca_file(PathBuf::from("test_certs/ca.pem"))
            .with_bind_address(([0, 0, 0, 0], 8883)),
        )
        .with_websocket(
            WebSocketConfig::new()
                .with_bind_address(([0, 0, 0, 0], 8080))
                .with_path("/mqtt")
                .with_tls(false),
        );

    // Create and run the broker
    let mut broker = MqttBroker::with_config(config).await?;

    info!("MQTT broker started with all transport types");
    info!("  Plain TCP:  mqtt://localhost:1883");
    info!("  TLS:        mqtts://localhost:8883");
    info!("  WebSocket:  ws://localhost:8080/mqtt");
    info!("Press Ctrl+C to stop");

    // Run until shutdown signal
    tokio::select! {
        result = broker.run() => {
            if let Err(e) = result {
                eprintln!("Broker error: {e}");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Shutting down...");
        }
    }

    Ok(())
}
