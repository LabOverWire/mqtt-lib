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

fn spawn_run(
    bridge: &Arc<BridgeConnection>,
) -> tokio::task::JoinHandle<mqtt5::broker::bridge::Result<()>> {
    let running = Arc::clone(bridge);
    tokio::spawn(async move { running.run().await })
}

async fn quic_bridge_connects(protocol: BridgeProtocol, insecure: Option<bool>) -> bool {
    let addr = free_udp_addr();
    let broker = quic_broker(addr).await;
    let mut config = BridgeConfig::new("quic-verify", addr.to_string()).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.protocol = protocol;
    config.insecure = insecure;
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.fallback_tcp = false;
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let task = spawn_run(&bridge);
    let mut connected = false;
    for _ in 0..60 {
        if bridge.get_stats().await.connected {
            connected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = bridge.stop().await;
    task.abort();
    broker.shutdown();
    connected
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quic_bridge_refuses_a_certificate_it_cannot_verify() {
    assert!(!quic_bridge_connects(BridgeProtocol::Quic, None).await);
    assert!(!quic_bridge_connects(BridgeProtocol::QuicSecure, None).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_insecure_quic_bridge_connects_without_verifying() {
    assert!(quic_bridge_connects(BridgeProtocol::Quic, Some(true)).await);
    assert!(quic_bridge_connects(BridgeProtocol::QuicSecure, Some(true)).await);
}
