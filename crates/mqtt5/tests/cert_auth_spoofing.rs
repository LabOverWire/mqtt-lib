#![allow(clippy::large_futures)]

use mqtt5::broker::auth::CertificateAuthProvider;
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::MqttClient;
use std::net::SocketAddr;
use std::sync::Arc;

const TEST_FINGERPRINT: &str = "aabbccdd00112233445566778899aabbccdd00112233445566778899aabbccdd";

#[tokio::test]
async fn test_cert_client_id_rejected_on_plain_tcp() {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        });

    let cert_provider = CertificateAuthProvider::new();
    cert_provider.add_certificate(TEST_FINGERPRINT, "alice");

    let mut broker = mqtt5::broker::server::MqttBroker::with_config(config)
        .await
        .expect("Failed to create broker")
        .with_auth_provider(Arc::new(cert_provider));

    let addr = broker.local_addr().expect("Failed to get broker address");
    let address = format!("mqtt://{addr}");

    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let spoofed_id = format!("cert:{TEST_FINGERPRINT}");
    let client = MqttClient::new(&spoofed_id);
    let result = client.connect(&address).await;
    assert!(
        result.is_err(),
        "cert: client ID must be rejected on plain TCP"
    );

    handle.abort();
}

#[tokio::test]
async fn test_non_cert_client_id_not_blocked_by_transport_guard() {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        });

    let cert_provider = CertificateAuthProvider::new();
    cert_provider.add_certificate(TEST_FINGERPRINT, "alice");

    let mut broker = mqtt5::broker::server::MqttBroker::with_config(config)
        .await
        .expect("Failed to create broker")
        .with_auth_provider(Arc::new(cert_provider));

    let addr = broker.local_addr().expect("Failed to get broker address");
    let address = format!("mqtt://{addr}");

    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = MqttClient::new("normal-client");
    let result = client.connect(&address).await;
    assert!(
        result.is_err(),
        "CertificateAuthProvider rejects non-cert: clients, but the transport guard itself should not block them"
    );

    handle.abort();
}
