#![cfg(feature = "broker")]
mod common;
use common::cli_helpers::run_cli_command;
use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection, BridgeProtocol};
use mqtt5::broker::config::{
    BrokerConfig, StorageBackend, StorageConfig, TlsConfig as BrokerTlsConfig,
};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient, QoS};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn certs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test_certs")
}

fn trusted_ca() -> Vec<u8> {
    std::fs::read(certs().join("ca.pem")).unwrap()
}

fn provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

async fn mqtts_broker() -> (u16, tokio::task::JoinHandle<()>) {
    provider();
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_tls(
            BrokerTlsConfig::new(certs().join("server.pem"), certs().join("server.key"))
                .with_require_client_cert(false)
                .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap()),
        );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let port = broker.tls_local_addr().unwrap().port();
    let mut ready = broker.ready_receiver();
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&v| v).await.unwrap();
    (port, handle)
}

fn client(id: &str) -> MqttClient {
    MqttClient::with_options(ConnectOptions::new(id).with_automatic_reconnect(false))
}

#[tokio::test]
async fn an_unloadable_ca_on_a_fresh_client_is_reported() {
    provider();
    let outcome = client("fresh-bad-ca")
        .set_tls_config(None, None, Some(b"not a certificate".to_vec()))
        .await;
    assert!(outcome.is_err(), "{outcome:?}");
}

#[tokio::test]
async fn an_unloadable_client_cert_is_reported_with_a_valid_ca() {
    provider();
    let outcome = client("bad-cert")
        .set_tls_config(
            Some(b"not a certificate".to_vec()),
            Some(b"not a key".to_vec()),
            Some(trusted_ca()),
        )
        .await;
    assert!(outcome.is_err(), "{outcome:?}");
}

#[tokio::test]
async fn an_unloadable_client_key_is_reported_with_a_valid_cert_and_ca() {
    provider();
    let outcome = client("bad-key")
        .set_tls_config(
            Some(std::fs::read(certs().join("client.pem")).unwrap()),
            Some(b"not a key".to_vec()),
            Some(trusted_ca()),
        )
        .await;
    assert!(outcome.is_err(), "{outcome:?}");
}

#[tokio::test]
async fn a_certificate_without_a_key_or_a_key_without_a_certificate_is_reported() {
    provider();
    let cert_only = client("cert-only")
        .set_tls_config(
            Some(std::fs::read(certs().join("client.pem")).unwrap()),
            None,
            None,
        )
        .await;
    let key_only = client("key-only")
        .set_tls_config(
            None,
            Some(std::fs::read(certs().join("client.key")).unwrap()),
            None,
        )
        .await;
    assert!(cert_only.is_err(), "{cert_only:?}");
    assert!(key_only.is_err(), "{key_only:?}");
}

#[tokio::test]
async fn a_certificate_without_a_key_leaves_the_stored_ca() {
    let (port, handle) = mqtts_broker().await;
    let client = client("cert-only-keeps-ca");
    client
        .set_tls_config(None, None, Some(trusted_ca()))
        .await
        .unwrap();
    let replaced = client
        .set_tls_config(
            Some(std::fs::read(certs().join("client.pem")).unwrap()),
            None,
            None,
        )
        .await;
    let connected = client.connect(&format!("mqtts://localhost:{port}")).await;
    let _ = client.disconnect().await;
    handle.abort();
    assert!(replaced.is_err(), "{replaced:?}");
    assert!(connected.is_ok(), "{connected:?}");
}

#[tokio::test]
async fn bench_connections_with_an_unloadable_ca_fails_with_the_load_error() {
    let ca_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(ca_file.path(), b"not a certificate").unwrap();
    let outcome = run_cli_command(&[
        "bench",
        "--mode",
        "connections",
        "--url",
        "mqtts://127.0.0.1:1",
        "--ca-cert",
        ca_file.path().to_str().unwrap(),
        "--concurrency",
        "2",
        "--duration",
        "1",
    ])
    .await;
    assert!(!outcome.success, "{}", outcome.stderr);
    assert!(
        outcome.contains("Failed to load TLS certificates"),
        "{}",
        outcome.stderr
    );
}

#[tokio::test]
async fn a_failed_replacement_is_reported_and_leaves_the_stored_config() {
    let (port, handle) = mqtts_broker().await;
    let client = client("replace-bad-ca");
    client
        .set_tls_config(None, None, Some(trusted_ca()))
        .await
        .unwrap();
    let replaced = client
        .set_tls_config(None, None, Some(b"not a certificate".to_vec()))
        .await;
    let connected = client.connect(&format!("mqtts://localhost:{port}")).await;
    let _ = client.disconnect().await;
    handle.abort();
    assert!(replaced.is_err(), "{replaced:?}");
    assert!(connected.is_ok(), "{connected:?}");
}

#[tokio::test]
async fn a_valid_config_is_stored_and_used() {
    let (port, handle) = mqtts_broker().await;
    let client = client("valid-ca");
    let stored = client.set_tls_config(None, None, Some(trusted_ca())).await;
    let connected = client.connect(&format!("mqtts://localhost:{port}")).await;
    let _ = client.disconnect().await;
    handle.abort();
    assert!(stored.is_ok(), "{stored:?}");
    assert!(connected.is_ok(), "{connected:?}");
}

#[tokio::test]
async fn a_quic_bridge_with_an_unloadable_ca_file_reports_it() {
    provider();
    let dir = tempfile::tempdir().unwrap();
    let ca_file = dir.path().join("ca.pem");
    std::fs::write(&ca_file, b"not a certificate").unwrap();
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut config = BridgeConfig::new("bad-ca-bridge", format!("127.0.0.1:{port}")).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.protocol = BridgeProtocol::QuicSecure;
    config.tls_server_name = Some("localhost".to_string());
    config.ca_file = Some(ca_file.to_string_lossy().into_owned());
    config.retry_jitter = false;
    let bridge =
        Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).expect("config"));
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move {
        let _ = running.run().await;
    });
    let mut last_error = None;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        last_error = bridge.get_stats().await.last_error;
        if last_error.is_some() {
            break;
        }
    }
    bridge.stop().await.ok();
    task.abort();
    let last_error = last_error.unwrap_or_default();
    assert!(
        last_error.contains("Failed to load TLS certificates"),
        "{last_error}"
    );
}
