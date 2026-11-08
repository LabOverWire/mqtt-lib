#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection, BridgeProtocol};
use mqtt5::broker::config::{QuicConfig, TlsConfig as BrokerTlsConfig};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, BrokerShutdownHandle, MqttBroker, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::task::JoinHandle;

fn certs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test_certs")
}

fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn spawn_run(bridge: &Arc<BridgeConnection>) -> JoinHandle<mqtt5::broker::bridge::Result<()>> {
    let running = Arc::clone(bridge);
    tokio::spawn(async move { running.run().await })
}

async fn panics_within(
    task: JoinHandle<mqtt5::broker::bridge::Result<()>>,
    wait: Duration,
) -> bool {
    matches!(tokio::time::timeout(wait, task).await, Ok(Err(join)) if join.is_panic())
}

fn unreachable_config(name: &str) -> BridgeConfig {
    let mut config = BridgeConfig::new(name, format!("127.0.0.1:{}", closed_port())).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.fallback_tcp = false;
    config.first_retry_delay = Duration::from_millis(50);
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_default_backoff_does_not_panic() {
    let mut config = unreachable_config("backoff-control");
    config.initial_reconnect_delay = Duration::from_millis(50);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let task = spawn_run(&bridge);
    let panicked = panics_within(task, Duration::from_secs(2)).await;
    let _ = bridge.stop().await;
    assert!(!panicked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_initial_reconnect_delay_does_not_panic() {
    let mut config = unreachable_config("backoff-long");
    config.initial_reconnect_delay = Duration::MAX;
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let task = spawn_run(&bridge);
    let panicked = panics_within(task, Duration::from_secs(2)).await;
    let _ = bridge.stop().await;
    assert!(!panicked, "run() panicked computing the reconnect delay");
}

async fn tls_broker() -> (SocketAddr, BrokerShutdownHandle) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls_addr: SocketAddr = format!("127.0.0.1:{}", closed_port()).parse().unwrap();
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
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    (tls_addr, shutdown)
}

async fn reaches_backup(primary: String, tls: bool, backup: SocketAddr) -> bool {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut config = BridgeConfig::new("tls-unresolvable", primary).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.fallback_tcp = false;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(50);
    config.backup_brokers = vec![backup.to_string()];
    if tls {
        config.use_tls = true;
        config.tls_server_name = Some("localhost".to_string());
        config.ca_file = Some(certs().join("ca.pem").to_string_lossy().into_owned());
    }
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let task = spawn_run(&bridge);
    let mut reached = false;
    for _ in 0..60 {
        let stats = bridge.get_stats().await;
        if stats.connected && !stats.on_primary {
            reached = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = bridge.stop().await;
    task.abort();
    reached
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_tls_bridge_with_a_closed_primary_reaches_its_backup() {
    let (backup, shutdown) = tls_broker().await;
    let reached = reaches_backup(format!("127.0.0.1:{}", closed_port()), true, backup).await;
    shutdown.shutdown();
    assert!(reached);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_bridge_with_an_unresolvable_primary_reaches_its_backup() {
    let (backup, shutdown) = tls_broker().await;
    let reached = reaches_backup("localhost:70000".to_string(), true, backup).await;
    shutdown.shutdown();
    assert!(reached, "the TLS bridge never tried its backup");
}

async fn quic_broker(addr: SocketAddr) -> BrokerShutdownHandle {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_storage(StorageConfig::new().with_persistence(false))
        .with_quic(
            QuicConfig::new(certs().join("server.pem"), certs().join("server.key"))
                .with_bind_address(addr),
        );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    shutdown
}

fn free_udp_addr() -> SocketAddr {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

async fn quic_fails_back() -> bool {
    let primary_addr = free_udp_addr();
    let backup_addr = free_udp_addr();
    let backup = quic_broker(backup_addr).await;
    let mut config = BridgeConfig::new("quic-failback", primary_addr.to_string()).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.protocol = BridgeProtocol::Quic;
    config.insecure = Some(true);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.fallback_tcp = false;
    config.first_retry_delay = Duration::from_millis(100);
    config.initial_reconnect_delay = Duration::from_millis(200);
    config.backup_brokers = vec![backup_addr.to_string()];
    config.primary_health_check_interval = Duration::from_millis(300);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let task = spawn_run(&bridge);
    let mut on_backup = false;
    for _ in 0..1200 {
        let stats = bridge.get_stats().await;
        if stats.connected && !stats.on_primary {
            on_backup = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(on_backup, "reached the QUIC backup");

    let primary = quic_broker(primary_addr).await;
    let mut on_primary = false;
    for _ in 0..400 {
        let stats = bridge.get_stats().await;
        if stats.connected && stats.on_primary {
            on_primary = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = bridge.stop().await;
    task.abort();
    primary.shutdown();
    backup.shutdown();
    on_primary
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quic_bridge_fails_back_to_its_primary() {
    assert!(
        quic_fails_back().await,
        "the QUIC bridge stayed on its backup 20s after the primary came back"
    );
}
