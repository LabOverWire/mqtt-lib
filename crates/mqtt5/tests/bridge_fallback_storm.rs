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
    ready.wait_for(|&r| r).await.unwrap();
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

async fn quic_only_broker(udp_port: u16) -> BrokerShutdownHandle {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let addr: SocketAddr = format!("127.0.0.1:{udp_port}").parse().unwrap();
    let quic = QuicConfig::new(certs().join("server.pem"), certs().join("server.key"))
        .with_bind_address(addr);
    run_broker(
        BrokerConfig::default()
            .with_bind_address(([127, 0, 0, 1], free_tcp_port()))
            .with_storage(StorageConfig::new().with_persistence(false))
            .with_quic(quic),
    )
    .await
}

async fn observe(
    protocol: BridgeProtocol,
    fallbacks: Vec<BridgeProtocol>,
    quic_primary: bool,
) -> (u64, bool) {
    let primary_port = if quic_primary {
        free_udp_port()
    } else {
        free_tcp_port()
    };
    let backup_port = free_tcp_port();
    let backup = tcp_broker(backup_port).await;
    let primary = if quic_primary {
        quic_only_broker(primary_port).await
    } else {
        tcp_broker(primary_port).await
    };
    let mut config = BridgeConfig::new("storm", format!("127.0.0.1:{primary_port}")).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.protocol = protocol;
    config.fallback_protocols = fallbacks;
    config.fallback_tcp = false;
    config.insecure = Some(true);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(100);
    config.initial_reconnect_delay = Duration::from_millis(200);
    config.max_reconnect_delay = Duration::from_secs(2);
    config.backup_brokers = vec![format!("127.0.0.1:{backup_port}")];
    config.primary_health_check_interval = Duration::from_millis(300);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    let mut ever_primary = false;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(16) {
        let s = bridge.get_stats().await;
        ever_primary |= s.connected && s.on_primary;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let failback_attempts = bridge.get_stats().await.failback_attempts;
    let _ = bridge.stop().await;
    task.abort();
    primary.shutdown();
    backup.shutdown();
    (failback_attempts, ever_primary)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tcp_bridge_with_quic_fallback_and_quic_only_primary_stays_on_backup() {
    let (failbacks, ever_primary) =
        observe(BridgeProtocol::Tcp, vec![BridgeProtocol::Quic], true).await;
    assert!(
        !ever_primary,
        "setup: the primary is reached only over the fallback"
    );
    assert_eq!(
        failbacks, 0,
        "a bridge on its backup over its main protocol was disconnected {failbacks} times in 16s for a primary that it can only reach over a fallback it never tries while the backup is up"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_quic_only_primary_is_reachable_over_quic() {
    let (failbacks, ever_primary) = observe(BridgeProtocol::Quic, vec![], true).await;
    assert!(
        ever_primary,
        "the QUIC-only primary accepts the bridge over QUIC"
    );
    assert_eq!(failbacks, 0);
}
