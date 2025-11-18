//! Simple MQTT v5.0 Broker Example
//!
//! This example demonstrates how to start and run a basic MQTT broker.
//! The broker will:
//! - Listen on localhost:1883 (standard MQTT port)
//! - Accept anonymous connections
//! - Support all MQTT v5.0 features
//! - Show basic logging of client connections
//!
//! ## Usage
//!
//! ```bash
//! # Run the broker
//! cargo run --example simple_broker
//!
//! # In another terminal, connect with any MQTT client:
//! # Using our mqttv5 CLI (recommended):
//! mqttv5 sub --host localhost --topic '#' --verbose
//! mqttv5 pub --host localhost --topic 'test/topic' --message 'Hello MQTT!'
//!
//! # Or with traditional mosquitto tools:
//! mosquitto_sub -h localhost -t '#' -v
//! mosquitto_pub -h localhost -t 'test/topic' -m 'Hello MQTT!'
//!
//! # Or run the simple_client example:
//! cargo run --example simple_client
//! ```

use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::time::Duration;
use std::net::SocketAddr;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing for logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("mqtt5=info")),
        )
        .init();

    println!("🚀 MQTT v5.0 Simple Broker Example");
    println!("==================================\n");

    // Create broker configuration
    let mut config = BrokerConfig {
        bind_addresses: vec!["127.0.0.1:1883".parse::<SocketAddr>()?],
        max_clients: 1000,
        max_packet_size: 10 * 1024 * 1024,                  // 10MB
        session_expiry_interval: Duration::from_secs(3600), // 1 hour
        maximum_qos: 2,
        retain_available: true,
        wildcard_subscription_available: true,
        subscription_identifier_available: true,
        shared_subscription_available: true,
        topic_alias_maximum: 100,
        ..Default::default()
    };

    // Authentication - allow anonymous for demo
    config.auth_config.allow_anonymous = true;

    info!("📋 Broker Configuration:");
    info!("   Address: {:?}", config.bind_addresses);
    info!("   Max clients: {}", config.max_clients);
    info!("   Max QoS: {}", config.maximum_qos);
    info!(
        "   Anonymous allowed: {}",
        config.auth_config.allow_anonymous
    );
    info!(
        "   Shared subscriptions: {}",
        config.shared_subscription_available
    );

    // Create and bind the broker
    let mut broker = MqttBroker::with_config(config).await?;

    info!("✅ MQTT broker listening on 127.0.0.1:1883");
    info!("📡 Clients can connect to mqtt://localhost:1883");
    info!("🛑 Press Ctrl+C to stop the broker\n");

    // Handle shutdown signal
    let shutdown = tokio::spawn(async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for ctrl-c");
        info!("\n⚠️  Shutdown signal received");
    });

    // Run the broker
    tokio::select! {
        result = broker.run() => {
            match result {
                Ok(()) => info!("Broker stopped normally"),
                Err(e) => error!("Broker error: {}", e),
            }
        }
        _ = shutdown => {
            info!("Shutting down broker...");
        }
    }

    info!("✅ Broker shutdown complete");
    Ok(())
}
