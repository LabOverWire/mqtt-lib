#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::sync::Arc;

async fn attempts_in_two_seconds(multiplier: f64) -> u64 {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let mut config = BridgeConfig::new("zero-mult", format!("127.0.0.1:{port}")).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.fallback_tcp = false;
    config.first_retry_delay = Duration::from_millis(500);
    config.initial_reconnect_delay = Duration::from_millis(500);
    config.backoff_multiplier = multiplier;
    assert!(config.validate().is_ok());
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    tokio::time::sleep(Duration::from_secs(2)).await;
    let attempts = bridge.get_stats().await.connection_attempts;
    let _ = bridge.stop().await;
    task.abort();
    attempts
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_multiplier_two_paces_reconnects() {
    let attempts = attempts_in_two_seconds(2.0).await;
    assert!(attempts <= 10);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_zero_multiplier_paces_reconnects() {
    let attempts = attempts_in_two_seconds(0.0).await;
    assert!(attempts <= 10, "{attempts} connection attempts in 2 s");
}
