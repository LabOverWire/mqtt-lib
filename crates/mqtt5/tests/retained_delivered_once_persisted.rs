#![cfg(feature = "broker")]

use mqtt5::broker::{BrokerConfig, MqttBroker, StorageConfig};
use mqtt5::client::MqttClient;
use mqtt5::time::Duration;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

async fn retained_deliveries(persist: bool, wildcard: bool, settle: u64) -> usize {
    let dir = tempfile::tempdir().unwrap();
    let config = BrokerConfig::default()
        .with_bind_address(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
        .with_max_clients(100)
        .with_storage(
            StorageConfig::new()
                .with_persistence(persist)
                .with_base_dir(dir.path().to_path_buf()),
        );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    let task = tokio::spawn(async move {
        let outcome = broker.run().await;
        assert!(outcome.is_ok(), "broker run failed: {outcome:?}");
    });
    while !*ready.borrow_and_update() {
        ready.changed().await.unwrap();
    }

    let publisher = MqttClient::new(format!("retained-once-pub-{persist}-{wildcard}-{settle}"));
    publisher.connect(&format!("mqtt://{addr}")).await.unwrap();
    publisher
        .publish_retain("sensors/a/temp", b"r".to_vec())
        .await
        .unwrap();
    publisher.disconnect().await.unwrap();
    tokio::time::sleep(Duration::from_millis(settle)).await;

    let received = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&received);
    let subscriber = MqttClient::new(format!("retained-once-sub-{persist}-{wildcard}-{settle}"));
    subscriber.connect(&format!("mqtt://{addr}")).await.unwrap();
    let filter = if wildcard {
        "sensors/#"
    } else {
        "sensors/a/temp"
    };
    subscriber
        .subscribe(filter, move |_| {
            sink.fetch_add(1, Ordering::SeqCst);
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let deliveries = received.load(Ordering::SeqCst);
    subscriber.disconnect().await.unwrap();
    shutdown.shutdown();
    task.await.unwrap();
    deliveries
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_once_in_memory_control() {
    assert_eq!(retained_deliveries(false, true, 0).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_once_with_persistence_exact_filter() {
    assert_eq!(retained_deliveries(true, false, 0).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_once_with_persistence_wildcard_filter() {
    assert_eq!(retained_deliveries(true, true, 0).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_once_with_persistence_after_settling_control() {
    assert_eq!(retained_deliveries(true, true, 1000).await, 1);
}
