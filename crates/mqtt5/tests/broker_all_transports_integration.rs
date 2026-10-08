#![cfg(feature = "broker")]
#![cfg(feature = "transport-websocket")]

use mqtt5::broker::config::{BrokerConfig, TlsConfig, WebSocketConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::time::Duration;
use std::path::PathBuf;

#[tokio::test]
async fn test_broker_all_transports() {
    let config = BrokerConfig::default()
        .with_storage(
            mqtt5::broker::config::StorageConfig::new()
                .with_backend(mqtt5::broker::config::StorageBackend::Memory),
        )
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_tls(
            TlsConfig::new(
                PathBuf::from("../../test_certs/server.crt"),
                PathBuf::from("../../test_certs/server.key"),
            )
            .with_bind_address(([127, 0, 0, 1], 0)),
        )
        .with_websocket(
            WebSocketConfig::new()
                .with_bind_address(([127, 0, 0, 1], 0))
                .with_path("/mqtt"),
        );

    let broker = MqttBroker::with_config(config).await;

    if broker.is_err() {
        eprintln!("Skipping all transports test - certificates not found");
        return;
    }

    let mut broker = broker.unwrap();

    let broker_handle = tokio::spawn(async move { broker.run().await });

    tokio::time::sleep(Duration::from_millis(200)).await;

    broker_handle.abort();
}

#[tokio::test]
async fn test_broker_transport_stats() {
    let config = BrokerConfig::default()
        .with_storage(
            mqtt5::broker::config::StorageConfig::new()
                .with_backend(mqtt5::broker::config::StorageBackend::Memory),
        )
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_websocket(
            WebSocketConfig::new()
                .with_bind_address(([127, 0, 0, 1], 0))
                .with_path("/mqtt"),
        );

    let broker = MqttBroker::with_config(config)
        .await
        .expect("Failed to create broker");

    let stats = broker.stats();
    assert_eq!(
        stats
            .clients_connected
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    let stats_clone = broker.stats();
    let mut broker = broker;
    let broker_handle = tokio::spawn(async move { broker.run().await });

    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(
        stats_clone
            .clients_connected
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    broker_handle.abort();
}
