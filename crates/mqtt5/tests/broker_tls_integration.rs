#![cfg(feature = "broker")]

use mqtt5::broker::config::{BrokerConfig, StorageConfig, TlsConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::time::timeout;

#[tokio::test]
async fn test_broker_tls_creation() {
    let config = BrokerConfig::default()
        .with_storage(
            mqtt5::broker::config::StorageConfig::new()
                .with_backend(mqtt5::broker::config::StorageBackend::Memory),
        )
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_tls(
            TlsConfig::new(
                PathBuf::from("../../test_certs/server.pem"),
                PathBuf::from("../../test_certs/server.key"),
            )
            .with_bind_address(([127, 0, 0, 1], 0)),
        );

    let broker = MqttBroker::with_config(config).await;

    if broker.is_err() {
        eprintln!("Skipping TLS test - certificates not found");
        return;
    }

    let mut broker = broker.unwrap();

    let broker_handle = tokio::spawn(async move { broker.run().await });

    tokio::time::sleep(Duration::from_millis(100)).await;

    broker_handle.abort();
}

#[tokio::test]
async fn test_broker_tls_only_no_plaintext() {
    let config = BrokerConfig::default()
        .with_bind_addresses(Vec::new())
        .with_storage(StorageConfig::new().with_persistence(false))
        .with_tls(
            TlsConfig::new(
                PathBuf::from("../../test_certs/server.pem"),
                PathBuf::from("../../test_certs/server.key"),
            )
            .with_bind_address(([127, 0, 0, 1], 0)),
        );

    let mut broker = MqttBroker::with_config(config)
        .await
        .expect("TLS-only broker should build");

    let tls_addr = broker
        .tls_local_addr()
        .expect("TLS-only broker should have a bound TLS listener");
    assert!(
        broker.local_addr().is_none(),
        "TLS-only broker must not bind a plaintext listener"
    );

    let mut ready_rx = broker.ready_receiver();
    let broker_handle = tokio::spawn(async move { broker.run().await });
    ready_rx
        .wait_for(|&ready| ready)
        .await
        .expect("broker ready signal should fire");

    let mut tls_config =
        mqtt5::transport::tls::TlsConfig::new(tls_addr, "localhost").with_verify_server_cert(false);
    tls_config
        .load_ca_cert_pem("../../test_certs/ca.pem")
        .expect("failed to load CA cert");

    let client = MqttClient::new("tls-only-client");
    timeout(
        Duration::from_secs(5),
        client.connect_with_tls_and_options(tls_config, ConnectOptions::default()),
    )
    .await
    .expect("TLS connection timed out")
    .expect("TLS connection failed");

    let received = Arc::new(Mutex::new(Vec::new()));
    let received_clone = Arc::clone(&received);
    client
        .subscribe("test/tls-only", move |msg| {
            received_clone.lock().unwrap().push(msg);
        })
        .await
        .expect("subscribe failed");

    client
        .publish("test/tls-only", b"tls-only roundtrip")
        .await
        .expect("publish failed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    {
        let msgs = received.lock().unwrap();
        assert_eq!(msgs.len(), 1, "expected exactly one delivered message");
        assert_eq!(msgs[0].topic, "test/tls-only");
        assert_eq!(&msgs[0].payload[..], b"tls-only roundtrip");
    }

    client.disconnect().await.expect("disconnect failed");
    broker_handle.abort();
}

#[tokio::test]
async fn test_broker_no_listeners_errors() {
    let config = BrokerConfig::default()
        .with_storage(
            mqtt5::broker::config::StorageConfig::new()
                .with_backend(mqtt5::broker::config::StorageBackend::Memory),
        )
        .with_bind_addresses(Vec::new());

    let broker = MqttBroker::with_config(config).await;

    assert!(
        broker.is_err(),
        "broker with no listeners should fail to build"
    );
}

#[tokio::test]
async fn test_broker_tls_with_client_certs() {
    let config = BrokerConfig::default()
        .with_storage(
            mqtt5::broker::config::StorageConfig::new()
                .with_backend(mqtt5::broker::config::StorageBackend::Memory),
        )
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_tls(
            TlsConfig::new(
                PathBuf::from("../../test_certs/server.pem"),
                PathBuf::from("../../test_certs/server.key"),
            )
            .with_ca_file(PathBuf::from("../../test_certs/ca.pem"))
            .with_require_client_cert(true)
            .with_bind_address(([127, 0, 0, 1], 0)),
        );

    let broker = MqttBroker::with_config(config).await;

    if broker.is_err() {
        eprintln!("Skipping TLS client cert test - certificates not found");
        return;
    }

    let mut broker = broker.unwrap();

    let broker_handle = tokio::spawn(async move { broker.run().await });

    tokio::time::sleep(Duration::from_millis(100)).await;
    broker_handle.abort();
}

#[tokio::test]
async fn test_broker_default_tls_port() {
    let config = BrokerConfig::default()
        .with_storage(
            mqtt5::broker::config::StorageConfig::new()
                .with_backend(mqtt5::broker::config::StorageBackend::Memory),
        )
        .with_bind_address(([127, 0, 0, 1], 1883))
        .with_tls(TlsConfig::new(
            PathBuf::from("../../test_certs/server.pem"),
            PathBuf::from("../../test_certs/server.key"),
        ));

    assert!(config.tls_config.is_some());
    let tls_config = config.tls_config.as_ref().unwrap();
    assert!(!tls_config.bind_addresses.is_empty());
    assert!(tls_config
        .bind_addresses
        .iter()
        .all(|addr| addr.port() == 8883));
}
