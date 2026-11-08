#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection, BridgeProtocol};
use mqtt5::broker::config::TlsConfig as BrokerTlsConfig;
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, BrokerShutdownHandle, MqttBroker, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

fn certs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test_certs")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn tls_broker(port: u16) -> BrokerShutdownHandle {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls_addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_storage(StorageConfig::new().with_persistence(false))
        .with_tls(
            BrokerTlsConfig::new(certs().join("server.pem"), certs().join("server.key"))
                .with_bind_address(tls_addr),
        );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&r| r).await.unwrap();
    shutdown
}

async fn outcome(primary_host: &str) -> (bool, u64) {
    let primary_port = free_port();
    let backup_port = free_port();
    let backup = tls_broker(backup_port).await;
    let mut config = BridgeConfig::new("tls-host", format!("{primary_host}:{primary_port}"))
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce);
    config.protocol = BridgeProtocol::Tls;
    config.tls_server_name = Some("localhost".to_string());
    config.ca_file = Some(certs().join("ca.pem").to_string_lossy().into_owned());
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.fallback_tcp = false;
    config.first_retry_delay = Duration::from_millis(100);
    config.initial_reconnect_delay = Duration::from_millis(200);
    config.max_reconnect_delay = Duration::from_millis(600);
    config.backup_brokers = vec![format!("127.0.0.1:{backup_port}")];
    config.primary_health_check_interval = Duration::from_millis(300);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    for _ in 0..200 {
        let s = bridge.get_stats().await;
        if s.connected && !s.on_primary {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let primary = tls_broker(primary_port).await;
    let mut on_primary = false;
    for _ in 0..160 {
        let s = bridge.get_stats().await;
        if s.connected && s.on_primary {
            on_primary = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let stats = bridge.get_stats().await;
    let _ = bridge.stop().await;
    task.abort();
    primary.shutdown();
    backup.shutdown();
    (on_primary, stats.failback_attempts)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_tls_ip_primary_fails_back() {
    let (on_primary, _) = outcome("127.0.0.1").await;
    assert!(on_primary);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tls_hostname_primary_fails_back() {
    let (on_primary, attempts) = outcome("localhost").await;
    assert!(
        on_primary,
        "a TLS bridge to localhost:port stayed on its backup after {attempts} failbacks"
    );
}
