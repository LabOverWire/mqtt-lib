#![cfg(feature = "broker")]

mod common;

use common::TestBroker;
use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient, QoS, SubscribeOptions};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use ulid::Ulid;

const SHARED: &str = "$share/g/a/b";
const PLAIN: &str = "a/b";

fn client_id(name: &str) -> String {
    format!("test-{name}-{}", Ulid::new())
}

fn qos1() -> SubscribeOptions {
    SubscribeOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    }
}

async fn wait_until(cond: impl Fn() -> bool) -> bool {
    for _ in 0..120 {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

async fn publish_to_plain_topic(broker: &TestBroker) {
    let publisher = MqttClient::new(client_id("shared-keys-pub"));
    publisher.connect(broker.address()).await.unwrap();
    publisher.publish_qos1(PLAIN, b"x".to_vec()).await.unwrap();
    publisher.disconnect().await.unwrap();
}

fn counting(counter: &Arc<AtomicU32>) -> impl Fn(mqtt5::types::Message) + Send + Sync + 'static {
    let counter = Arc::clone(counter);
    move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    }
}

async fn remaining_callback_receives(kept: &str, removed: Option<&str>) -> bool {
    let broker = TestBroker::start().await;
    let client = MqttClient::new(client_id("shared-keys"));
    client.connect(broker.address()).await.unwrap();
    let kept_count = Arc::new(AtomicU32::new(0));
    let removed_count = Arc::new(AtomicU32::new(0));
    client.subscribe(kept, counting(&kept_count)).await.unwrap();
    if let Some(removed) = removed {
        client
            .subscribe(removed, counting(&removed_count))
            .await
            .unwrap();
        client.unsubscribe(removed).await.unwrap();
    }

    publish_to_plain_topic(&broker).await;
    let delivered = wait_until(|| kept_count.load(Ordering::SeqCst) > 0).await;
    assert_eq!(removed_count.load(Ordering::SeqCst), 0);
    client.disconnect().await.unwrap();
    delivered
}

async fn remaining_ack_callback_receives(kept: &str, removed: Option<&str>) -> bool {
    let broker = TestBroker::start().await;
    let client = MqttClient::with_options(
        ConnectOptions::new(client_id("shared-ack"))
            .with_deferred_ack(true)
            .with_clean_start(false)
            .with_session_expiry_interval(60)
            .with_receive_maximum(16),
    );
    client.connect(broker.address()).await.unwrap();
    let kept_count = Arc::new(AtomicU32::new(0));
    let removed_count = Arc::new(AtomicU32::new(0));
    let kept_sink = Arc::clone(&kept_count);
    client
        .subscribe_with_ack(kept, qos1(), move |_, token| {
            kept_sink.fetch_add(1, Ordering::SeqCst);
            token.ack();
        })
        .await
        .unwrap();
    if let Some(removed) = removed {
        let removed_sink = Arc::clone(&removed_count);
        client
            .subscribe_with_ack(removed, qos1(), move |_, token| {
                removed_sink.fetch_add(1, Ordering::SeqCst);
                token.ack();
            })
            .await
            .unwrap();
        client.unsubscribe(removed).await.unwrap();
    }

    publish_to_plain_topic(&broker).await;
    let delivered = wait_until(|| kept_count.load(Ordering::SeqCst) > 0).await;
    assert_eq!(removed_count.load(Ordering::SeqCst), 0);
    client.disconnect().await.unwrap();
    delivered
}

#[tokio::test]
async fn shared_callback_receives_without_a_plain_subscription_control() {
    assert!(remaining_callback_receives(SHARED, None).await);
}

#[tokio::test]
async fn shared_ack_callback_receives_without_a_plain_subscription_control() {
    assert!(remaining_ack_callback_receives(SHARED, None).await);
}

#[tokio::test]
async fn unsubscribing_a_filter_keeps_the_shared_callback_for_it() {
    assert!(
        remaining_callback_receives(SHARED, Some(PLAIN)).await,
        "after unsubscribing {PLAIN}, the {SHARED} callback got nothing"
    );
}

#[tokio::test]
async fn unsubscribing_a_shared_filter_keeps_the_plain_callback_for_it() {
    assert!(
        remaining_callback_receives(PLAIN, Some(SHARED)).await,
        "after unsubscribing {SHARED}, the {PLAIN} callback got nothing"
    );
}

#[tokio::test]
async fn unsubscribing_a_filter_keeps_the_shared_ack_callback_for_it() {
    assert!(
        remaining_ack_callback_receives(SHARED, Some(PLAIN)).await,
        "after unsubscribing {PLAIN}, the {SHARED} ack callback got nothing"
    );
}

#[tokio::test]
async fn unsubscribing_a_shared_filter_keeps_the_plain_ack_callback_for_it() {
    assert!(
        remaining_ack_callback_receives(PLAIN, Some(SHARED)).await,
        "after unsubscribing {SHARED}, the {PLAIN} ack callback got nothing"
    );
}
