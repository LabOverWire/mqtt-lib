#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, MqttBroker, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;

static PANICS: AtomicUsize = AtomicUsize::new(0);

async fn backup_broker() -> SocketAddr {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_longest_health_check_interval_does_not_end_the_health_check() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICS.fetch_add(1, Ordering::SeqCst);
        previous(info);
    }));
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_port = closed.local_addr().unwrap().port();
    drop(closed);
    let backup = backup_broker().await;
    let mut config = BridgeConfig::new("longest-interval", format!("127.0.0.1:{primary_port}"))
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.backup_brokers = vec![backup.to_string()];
    config.primary_health_check_interval = Duration::MAX;
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    for _ in 0..300 {
        if bridge.get_stats().await.connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(bridge.get_stats().await.connected, "reached the backup");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = bridge.stop().await;
    task.abort();
    assert_eq!(PANICS.load(Ordering::SeqCst), 0, "a task panicked");
}
