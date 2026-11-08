#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection, BridgeProtocol};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, BrokerShutdownHandle, MqttBroker, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::sync::Arc;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn tcp_broker(port: u16) -> BrokerShutdownHandle {
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], port))
        .with_storage(StorageConfig::new().with_persistence(false));
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&r| r).await.unwrap();
    shutdown
}

async fn fails_back(protocol: BridgeProtocol) -> bool {
    let primary_port = free_port();
    let backup_port = free_port();
    let backup = tcp_broker(backup_port).await;
    let mut config = BridgeConfig::new("fallback-failback", format!("127.0.0.1:{primary_port}"))
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce);
    config.protocol = protocol;
    config.insecure = Some(true);
    config.fallback_tcp = true;
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(100);
    config.initial_reconnect_delay = Duration::from_millis(200);
    config.backup_brokers = vec![format!("127.0.0.1:{backup_port}")];
    config.primary_health_check_interval = Duration::from_millis(300);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    let mut on_backup = false;
    for _ in 0..2400 {
        let s = bridge.get_stats().await;
        if s.connected && !s.on_primary {
            on_backup = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(on_backup, "reached the backup over the TCP fallback");
    let primary = tcp_broker(primary_port).await;
    let mut on_primary = false;
    for _ in 0..2600 {
        let s = bridge.get_stats().await;
        if s.connected && s.on_primary {
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
async fn control_tcp_bridge_fails_back() {
    assert!(fails_back(BridgeProtocol::Tcp).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quic_bridge_on_its_tcp_fallback_fails_back() {
    assert!(
        fails_back(BridgeProtocol::Quic).await,
        "a QUIC bridge that reached its backup over the TCP fallback stayed there 130s after a TCP-only primary came back"
    );
}
