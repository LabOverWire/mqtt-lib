#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection, BridgeProtocol};
use mqtt5::broker::config::QuicConfig;
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, BrokerShutdownHandle, MqttBroker, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

fn certs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test_certs")
}

async fn tcp_broker() -> SocketAddr {
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_storage(StorageConfig::new().with_persistence(false));
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    addr
}

async fn freezable_proxy(upstream: SocketAddr, frozen: Arc<AtomicBool>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            let frozen = Arc::clone(&frozen);
            tokio::spawn(async move {
                let Ok(server) = tokio::net::TcpStream::connect(upstream).await else {
                    return;
                };
                let (mut cr, mut cw) = client.into_split();
                let (mut sr, mut sw) = server.into_split();
                let up_frozen = Arc::clone(&frozen);
                let up = tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    while let Ok(n) = cr.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        if !up_frozen.load(Ordering::SeqCst)
                            && sw.write_all(&buf[..n]).await.is_err()
                        {
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                });
                let mut buf = vec![0u8; 4096];
                while let Ok(n) = sr.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    if !frozen.load(Ordering::SeqCst) && cw.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_secs(3600)).await;
                drop(cw);
                up.abort();
            });
        }
    });
    port
}

fn spawn_run(bridge: &Arc<BridgeConnection>) -> JoinHandle<mqtt5::broker::bridge::Result<()>> {
    let running = Arc::clone(bridge);
    tokio::spawn(async move { running.run().await })
}

async fn wait_until(bridge: &BridgeConnection, on_primary: bool, secs: u64) -> bool {
    for _ in 0..secs * 20 {
        let stats = bridge.get_stats().await;
        if stats.connected && stats.on_primary == on_primary {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bridge_cut_off_from_a_frozen_remote_does_not_report_itself_connected() {
    let frozen = Arc::new(AtomicBool::new(false));
    let primary_port = freezable_proxy(tcp_broker().await, Arc::clone(&frozen)).await;
    let mut config = BridgeConfig::new("frozen", format!("127.0.0.1:{primary_port}")).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.fallback_tcp = false;
    config.keepalive = 2;
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let task = spawn_run(&bridge);
    assert!(wait_until(&bridge, true, 10).await, "connected to primary");
    frozen.store(true, Ordering::SeqCst);
    let mut detected = false;
    for _ in 0..200 {
        if bridge.get_stats().await.connection_attempts == 2 {
            detected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        detected,
        "the bridge noticed the lost connection and is reconnecting"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let stats = bridge.get_stats().await;
    let _ = bridge.stop().await;
    task.abort();
    assert!(
        !stats.connected,
        "2s into reconnecting after a lost connection, get_stats() says connected: {stats:?}"
    );
}

async fn quic_broker(addr: SocketAddr) -> (BrokerShutdownHandle, JoinHandle<()>) {
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
    let task = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    (shutdown, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quic_bridge_reconnects_after_its_remote_restarts() {
    let addr = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let (shutdown, broker_task) = quic_broker(addr).await;
    let mut config = BridgeConfig::new("quic-restart", addr.to_string()).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.protocol = BridgeProtocol::Quic;
    config.insecure = Some(true);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.fallback_tcp = false;
    config.keepalive = 2;
    config.first_retry_delay = Duration::from_millis(200);
    config.initial_reconnect_delay = Duration::from_millis(500);
    config.max_reconnect_delay = Duration::from_secs(2);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let task = spawn_run(&bridge);
    assert!(wait_until(&bridge, true, 15).await, "connected over QUIC");
    let first = bridge.get_stats().await.connected_since;

    shutdown.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(5), broker_task).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let (shutdown, _broker_task) = quic_broker(addr).await;

    let mut reconnected = false;
    for _ in 0..1200 {
        let stats = bridge.get_stats().await;
        if stats.connected && stats.connected_since != first {
            reconnected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let stats = bridge.get_stats().await;
    let _ = bridge.stop().await;
    task.abort();
    shutdown.shutdown();
    assert!(
        reconnected,
        "QUIC bridge did not reconnect within 60s: {stats:?}"
    );
}
