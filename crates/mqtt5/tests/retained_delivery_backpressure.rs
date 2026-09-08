#![cfg(feature = "broker")]
use mqtt5::broker::config::StorageConfig;
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::{MqttClient, PublishOptions, QoS};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const CHANNEL_CAPACITY: usize = 4;
const RETAINED_TOPICS: usize = 9;

async fn start_broker() -> (u16, tokio::task::JoinHandle<mqtt5::error::Result<()>>) {
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_client_channel_capacity(CHANNEL_CAPACITY)
        .with_storage(StorageConfig::default().with_persistence(false));
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let port = broker.local_addr().unwrap().port();
    let task = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (port, task)
}

#[tokio::test]
async fn subscribe_with_more_retained_messages_than_channel_capacity_completes() {
    let (port, broker_task) = start_broker().await;
    let url = format!("mqtt://127.0.0.1:{port}");

    let publisher = MqttClient::new("retained-publisher");
    publisher.connect(&url).await.unwrap();
    for index in 0..RETAINED_TOPICS {
        let options = PublishOptions {
            qos: QoS::AtMostOnce,
            retain: true,
            ..Default::default()
        };
        publisher
            .publish_with_options(format!("probe/ret/{index}"), b"v".to_vec(), options)
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let received = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&received);
    let subscriber = MqttClient::new("retained-subscriber");
    subscriber.connect(&url).await.unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        subscriber
            .subscribe("probe/ret/#", move |_msg| {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .await
            .unwrap();
    })
    .await
    .expect("SUBACK must arrive even when retained messages exceed the delivery channel");

    tokio::time::timeout(Duration::from_secs(5), async {
        subscriber
            .publish_qos1("probe/other", b"alive".to_vec())
            .await
            .unwrap();
    })
    .await
    .expect("the handler must keep servicing the socket after retained delivery");

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        received.load(Ordering::SeqCst) >= CHANNEL_CAPACITY,
        "retained messages that fit the channel must be delivered"
    );

    subscriber.disconnect().await.unwrap();
    publisher.disconnect().await.unwrap();
    broker_task.abort();
}
