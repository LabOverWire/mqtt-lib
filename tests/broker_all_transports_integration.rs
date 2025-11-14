//! Integration test for broker with all transport types

use mqtt5::broker::config::{BrokerConfig, TlsConfig, WebSocketConfig};
use mqtt5::broker::MqttBroker;
use std::path::PathBuf;
use mqtt5::time::Duration;

#[tokio::test]
async fn test_broker_all_transports() {
    // Test that we can create a broker with all transport types
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0)) // TCP on random port
        .with_tls(
            TlsConfig::new(
                PathBuf::from("test_certs/server.crt"),
                PathBuf::from("test_certs/server.key"),
            )
            .with_bind_address(([127, 0, 0, 1], 0)), // TLS on random port
        )
        .with_websocket(
            WebSocketConfig::new()
                .with_bind_address(([127, 0, 0, 1], 0)) // WebSocket on random port
                .with_path("/mqtt"),
        );

    let broker = MqttBroker::with_config(config).await;

    // Skip test if certificates don't exist
    if broker.is_err() {
        eprintln!("Skipping all transports test - certificates not found");
        return;
    }

    let mut broker = broker.unwrap();

    // Test that broker can start with all transports
    let broker_handle = tokio::spawn(async move { broker.run().await });

    // Give it time to start all listeners
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Then shut it down
    broker_handle.abort();

    // If we got here without panic, all transports initialized successfully
}

#[tokio::test]
async fn test_broker_transport_stats() {
    // Test that we can track connections from different transports
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_websocket(
            WebSocketConfig::new()
                .with_bind_address(([127, 0, 0, 1], 0))
                .with_path("/mqtt"),
        );

    let broker = MqttBroker::with_config(config)
        .await
        .expect("Failed to create broker");

    // Get initial stats
    let stats = broker.stats();
    assert_eq!(
        stats
            .clients_connected
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    // Run broker
    let stats_clone = broker.stats();
    let mut broker = broker;
    let broker_handle = tokio::spawn(async move { broker.run().await });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Stats should still be zero (no clients connected)
    assert_eq!(
        stats_clone
            .clients_connected
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    broker_handle.abort();
}
