mod common;

use common::TestBroker;
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::MqttClient;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

#[tokio::test]
async fn test_outbound_rate_limits_qos0_delivery() {
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };

    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config)
        .with_max_outbound_rate_per_client(10);

    let mut broker = TestBroker::start_with_config(config).await;

    let subscriber = MqttClient::new("rate-sub");
    subscriber.connect(broker.address()).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = Arc::clone(&received);

    subscriber
        .subscribe("#", move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = MqttClient::new("rate-pub");
    publisher.connect(broker.address()).await.unwrap();

    for _ in 0..50u32 {
        publisher.publish("test/rate", "x").await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let count = received.load(Ordering::Relaxed);
    assert!(
        count <= 12,
        "expected at most ~10 messages (with small margin), got {count}"
    );
    assert!(count >= 1, "expected at least 1 message, got {count}");

    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
    broker.stop().await;
}
