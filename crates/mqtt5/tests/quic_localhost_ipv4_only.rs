#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::config::{BrokerConfig, QuicConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

async fn quic_broker_on_ipv4_loopback() -> (u16, tokio::task::JoinHandle<()>, tempfile::TempDir) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cert.pem"), certified.cert.pem()).unwrap();
    std::fs::write(
        dir.path().join("key.pem"),
        certified.signing_key.serialize_pem(),
    )
    .unwrap();
    let config = BrokerConfig::default()
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_quic(
            QuicConfig::new(dir.path().join("cert.pem"), dir.path().join("key.pem"))
                .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap()),
        );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let port = broker.quic_local_addr().unwrap().port();
    let mut ready = broker.ready_receiver();
    let running = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    (port, running, dir)
}

#[tokio::test]
async fn quic_to_localhost_reaches_an_ipv4_only_broker_promptly() {
    let (port, running, _dir) = quic_broker_on_ipv4_loopback().await;
    let resolved: Vec<SocketAddr> = tokio::net::lookup_host(("localhost", port))
        .await
        .unwrap()
        .collect();
    tracing::info!(?resolved, "localhost resolves to");
    assert!(
        resolved.first().is_some_and(SocketAddr::is_ipv6),
        "precondition: localhost resolves to ::1 first, got {resolved:?}"
    );
    let client = MqttClient::new("ipv4-only-localhost");
    client.set_insecure_tls(true).await;
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(45),
        client.connect_with_options(
            &format!("quic://localhost:{port}"),
            ConnectOptions::new("ipv4-only-localhost").with_automatic_reconnect(false),
        ),
    )
    .await;
    let elapsed = started.elapsed();
    let _ = client.disconnect().await;
    running.abort();
    assert!(
        matches!(result, Ok(Ok(_))) && elapsed < Duration::from_secs(5),
        "resolved {resolved:?}, connect result {result:?} after {elapsed:?}"
    );
}
