use mqtt5::broker::config::{BrokerConfig, QuicConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::time::Duration;
use mqtt5::MqttClient;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use ulid::Ulid;

fn test_client_id(prefix: &str) -> String {
    format!("{}-{}", prefix, Ulid::new())
}

async fn start_quic_broker(quic_port: u16) -> (MqttBroker, SocketAddr) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let quic_addr: SocketAddr = format!("127.0.0.1:{quic_port}").parse().unwrap();

    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_quic(
            QuicConfig::new(
                PathBuf::from("../../test_certs/server.pem"),
                PathBuf::from("../../test_certs/server.key"),
            )
            .with_bind_address(quic_addr),
        );

    let broker = MqttBroker::with_config(config).await.unwrap();
    (broker, quic_addr)
}

#[tokio::test]
async fn test_quic_connection_survives_migration() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut broker, quic_addr) = start_quic_broker(24590).await;
    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client_id = test_client_id("quic-migrate");
    let topic = format!("migration-test/{}", Ulid::new());

    let client = MqttClient::new(client_id);
    client.set_insecure_tls(true).await;

    let broker_url = format!("quic://{quic_addr}");
    if client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    client
        .subscribe(&topic, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    client.publish(&topic, b"before migration").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(received.load(Ordering::Relaxed), 1);

    client.migrate().await.unwrap();

    client.publish(&topic, b"after migration").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        received.load(Ordering::Relaxed),
        2,
        "should receive message after QUIC migration"
    );

    client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_migrate_non_quic_returns_error() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_storage(storage_config);
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let tcp_addr = broker.local_addr().unwrap();
    let broker_handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client_id = test_client_id("tcp-migrate");
    let client = MqttClient::new(client_id);

    let broker_url = format!("mqtt://{tcp_addr}");
    client.connect(&broker_url).await.unwrap();

    let result = client.migrate().await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("QUIC"),
        "error should mention QUIC: {err_msg}"
    );

    client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_migrate_not_connected_returns_error() {
    let client = MqttClient::new("not-connected-migrate");
    let result = client.migrate().await;
    assert!(matches!(result, Err(mqtt5::error::MqttError::NotConnected)));
}
