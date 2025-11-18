//! Example of running an MQTT broker with TLS support
//!
//! This example demonstrates how to configure and run a broker that accepts
//! both plain TCP connections (port 1883) and TLS connections (port 8883).

use mqtt5::broker::config::TlsConfig;
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
    tracing_subscriber::fmt::init();

    // Create broker configuration with TLS
    let config = BrokerConfig::default()
        .with_bind_address(([0, 0, 0, 0], 1883))
        .with_tls(
            TlsConfig::new(
                PathBuf::from("certs/server.crt"),
                PathBuf::from("certs/server.key"),
            )
            .with_ca_file(PathBuf::from("certs/ca.crt"))
            .with_bind_address(([0, 0, 0, 0], 8883)),
        );

    // Create and run the broker
    let mut broker = MqttBroker::with_config(config).await?;

    info!("MQTT broker started");
    info!("  Plain TCP: mqtt://localhost:1883");
    info!("  TLS: mqtts://localhost:8883");
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
