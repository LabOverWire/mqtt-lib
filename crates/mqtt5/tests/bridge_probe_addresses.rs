#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection, BridgeProtocol};
use mqtt5::broker::config::QuicConfig;
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

fn free_tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn run_broker(config: BrokerConfig) -> BrokerShutdownHandle {
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    shutdown
}

async fn tcp_broker(port: u16) -> BrokerShutdownHandle {
    run_broker(
        BrokerConfig::default()
            .with_bind_address(([127, 0, 0, 1], port))
            .with_storage(StorageConfig::new().with_persistence(false)),
    )
    .await
}

async fn quic_broker(port: u16, require_client_cert: bool) -> BrokerShutdownHandle {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut quic = QuicConfig::new(certs().join("server.pem"), certs().join("server.key"))
        .with_bind_address(addr);
    if require_client_cert {
        quic.ca_file = Some(certs().join("ca.pem"));
        quic.require_client_cert = true;
    }
    run_broker(
        BrokerConfig::default()
            .with_bind_address(([127, 0, 0, 1], 0))
            .with_storage(StorageConfig::new().with_persistence(false))
            .with_quic(quic),
    )
    .await
}

fn base_config(name: &str, primary: String, backup: String) -> BridgeConfig {
    let mut config =
        BridgeConfig::new(name, primary).add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.fallback_tcp = false;
    config.first_retry_delay = Duration::from_millis(100);
    config.initial_reconnect_delay = Duration::from_millis(200);
    config.backup_brokers = vec![backup];
    config.primary_health_check_interval = Duration::from_millis(300);
    config
}

async fn fails_back(
    config: BridgeConfig,
    start_primary: impl std::future::Future<Output = BrokerShutdownHandle>,
) -> bool {
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    let mut on_backup = false;
    for _ in 0..1400 {
        let stats = bridge.get_stats().await;
        if stats.connected && !stats.on_primary {
            on_backup = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(on_backup, "reached the backup");
    let primary = start_primary.await;
    let mut on_primary = false;
    for _ in 0..600 {
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
    on_primary
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_tcp_ip_primary_fails_back() {
    let primary_port = free_tcp_port();
    let backup_port = free_tcp_port();
    let backup = tcp_broker(backup_port).await;
    let config = base_config(
        "probe-tcp-ip",
        format!("127.0.0.1:{primary_port}"),
        format!("127.0.0.1:{backup_port}"),
    );
    let ok = Box::pin(fails_back(config, tcp_broker(primary_port))).await;
    backup.shutdown();
    assert!(ok, "control failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tcp_hostname_primary_fails_back() {
    let primary_port = free_tcp_port();
    let backup_port = free_tcp_port();
    let backup = tcp_broker(backup_port).await;
    let config = base_config(
        "probe-tcp-host",
        format!("localhost:{primary_port}"),
        format!("127.0.0.1:{backup_port}"),
    );
    let ok = Box::pin(fails_back(config, tcp_broker(primary_port))).await;
    backup.shutdown();
    assert!(ok, "a bridge whose primary is localhost:port stayed on its backup 15s after the primary came back");
}

fn quic_config(name: &str, primary: String, backup: String) -> BridgeConfig {
    let mut config = base_config(name, primary, backup);
    config.protocol = BridgeProtocol::Quic;
    config.insecure = Some(true);
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_quic_ip_primary_fails_back() {
    let primary_port = free_udp_port();
    let backup_port = free_udp_port();
    let backup = quic_broker(backup_port, false).await;
    let config = quic_config(
        "probe-quic-ip",
        format!("127.0.0.1:{primary_port}"),
        format!("127.0.0.1:{backup_port}"),
    );
    let ok = Box::pin(fails_back(config, quic_broker(primary_port, false))).await;
    backup.shutdown();
    assert!(ok, "control failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quic_hostname_primary_fails_back() {
    let primary_port = free_udp_port();
    let backup_port = free_udp_port();
    let backup = quic_broker(backup_port, false).await;
    let config = quic_config(
        "probe-quic-host",
        format!("localhost:{primary_port}"),
        format!("127.0.0.1:{backup_port}"),
    );
    let ok = Box::pin(fails_back(config, quic_broker(primary_port, false))).await;
    backup.shutdown();
    assert!(ok, "a QUIC bridge whose primary is localhost:port stayed on its backup 15s after the primary came back");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quic_mtls_primary_fails_back() {
    let primary_port = free_udp_port();
    let backup_port = free_udp_port();
    let backup = quic_broker(backup_port, true).await;
    let mut config = quic_config(
        "probe-quic-mtls",
        format!("127.0.0.1:{primary_port}"),
        format!("127.0.0.1:{backup_port}"),
    );
    config.ca_file = Some(certs().join("ca.pem").to_string_lossy().into_owned());
    config.client_cert_file = Some(certs().join("client.pem").to_string_lossy().into_owned());
    config.client_key_file = Some(certs().join("client.key").to_string_lossy().into_owned());
    let ok = Box::pin(fails_back(config, quic_broker(primary_port, true))).await;
    backup.shutdown();
    assert!(ok, "a QUIC bridge whose primary requires a client certificate stayed on its backup 15s after the primary came back");
}
