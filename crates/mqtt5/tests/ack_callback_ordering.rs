#![cfg(feature = "broker")]

mod common;

use common::TestBroker;
use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient, QoS, SubscribeOptions};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use ulid::Ulid;

const PLAIN: &str = "r363/a/b";
const SHARED: &str = "$share/g/r363/a/b";
const G1: &str = "$share/g1/r363/a/b";
const G2: &str = "$share/g2/r363/a/b";

fn client_id(name: &str) -> String {
    format!("r363-{name}-{}", Ulid::new())
}

fn qos1() -> SubscribeOptions {
    SubscribeOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    }
}

async fn publish_once(broker: &TestBroker) {
    let publisher = MqttClient::new(client_id("pub"));
    publisher.connect(broker.address()).await.unwrap();
    publisher.publish_qos1(PLAIN, b"x".to_vec()).await.unwrap();
    publisher.disconnect().await.unwrap();
}

fn ack_client() -> MqttClient {
    MqttClient::with_options(
        ConnectOptions::new(client_id("ack"))
            .with_deferred_ack(true)
            .with_clean_start(false)
            .with_session_expiry_interval(60)
            .with_receive_maximum(16),
    )
}

async fn ack_sub(client: &MqttClient, filter: &str, counter: &Arc<AtomicU32>) {
    let sink = Arc::clone(counter);
    client
        .subscribe_with_ack(filter, qos1(), move |_, token| {
            sink.fetch_add(1, Ordering::SeqCst);
            token.ack();
        })
        .await
        .unwrap();
}

fn counting(counter: &Arc<AtomicU32>) -> impl Fn(mqtt5::types::Message) + Send + Sync + 'static {
    let counter = Arc::clone(counter);
    move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    }
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(700)).await;
}

#[tokio::test]
async fn unsubscribing_one_share_group_keeps_the_other() {
    let broker = TestBroker::start().await;
    let client = MqttClient::new(client_id("groups"));
    client.connect(broker.address()).await.unwrap();
    let c1 = Arc::new(AtomicU32::new(0));
    let c2 = Arc::new(AtomicU32::new(0));
    client.subscribe(G1, counting(&c1)).await.unwrap();
    client.subscribe(G2, counting(&c2)).await.unwrap();
    client.unsubscribe(G1).await.unwrap();
    publish_once(&broker).await;
    settle().await;
    let counts = (c1.load(Ordering::SeqCst), c2.load(Ordering::SeqCst));
    client.disconnect().await.unwrap();
    assert_eq!(counts, (0, 1), "g1, g2 counts");
}

#[tokio::test]
async fn ack_unsubscribing_one_share_group_keeps_the_other() {
    let broker = TestBroker::start().await;
    let client = ack_client();
    client.connect(broker.address()).await.unwrap();
    let c1 = Arc::new(AtomicU32::new(0));
    let c2 = Arc::new(AtomicU32::new(0));
    ack_sub(&client, G1, &c1).await;
    ack_sub(&client, G2, &c2).await;
    client.unsubscribe(G1).await.unwrap();
    publish_once(&broker).await;
    settle().await;
    let counts = (c1.load(Ordering::SeqCst), c2.load(Ordering::SeqCst));
    client.disconnect().await.unwrap();
    assert_eq!(counts, (0, 1), "g1, g2 counts");
}

#[tokio::test]
async fn ack_latest_subscription_receives_when_shared_and_plain_coexist() {
    let broker = TestBroker::start().await;
    let client = ack_client();
    client.connect(broker.address()).await.unwrap();
    let plain = Arc::new(AtomicU32::new(0));
    let shared = Arc::new(AtomicU32::new(0));
    ack_sub(&client, PLAIN, &plain).await;
    ack_sub(&client, SHARED, &shared).await;
    publish_once(&broker).await;
    settle().await;
    let counts = (plain.load(Ordering::SeqCst), shared.load(Ordering::SeqCst));
    client.disconnect().await.unwrap();
    assert_eq!(counts, (0, 2), "plain, shared counts");
}

#[tokio::test]
async fn ack_resubscribed_plain_receives_after_shared_was_kept() {
    let broker = TestBroker::start().await;
    let client = ack_client();
    client.connect(broker.address()).await.unwrap();
    let first = Arc::new(AtomicU32::new(0));
    let shared = Arc::new(AtomicU32::new(0));
    let again = Arc::new(AtomicU32::new(0));
    ack_sub(&client, PLAIN, &first).await;
    ack_sub(&client, SHARED, &shared).await;
    client.unsubscribe(PLAIN).await.unwrap();
    ack_sub(&client, PLAIN, &again).await;
    publish_once(&broker).await;
    settle().await;
    let counts = (
        first.load(Ordering::SeqCst),
        shared.load(Ordering::SeqCst),
        again.load(Ordering::SeqCst),
    );
    client.disconnect().await.unwrap();
    assert_eq!(counts, (0, 0, 2), "first plain, shared, resubscribed plain");
}

#[tokio::test]
async fn plain_and_ack_shared_unsubscribe_ack_keeps_plain() {
    let broker = TestBroker::start().await;
    let client = ack_client();
    client.connect(broker.address()).await.unwrap();
    let plain = Arc::new(AtomicU32::new(0));
    let shared = Arc::new(AtomicU32::new(0));
    client.subscribe(PLAIN, counting(&plain)).await.unwrap();
    ack_sub(&client, SHARED, &shared).await;
    client.unsubscribe(SHARED).await.unwrap();
    publish_once(&broker).await;
    settle().await;
    let counts = (plain.load(Ordering::SeqCst), shared.load(Ordering::SeqCst));
    client.disconnect().await.unwrap();
    assert_eq!(counts, (1, 0), "plain, shared counts");
}
