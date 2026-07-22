#![cfg(feature = "broker")]
#![allow(clippy::large_futures)]

mod common;

use common::TestBroker;
use mqtt5::time::Duration;
use mqtt5::{AckToken, ConnectOptions, MqttClient, PublishOptions, QoS, SubscribeOptions};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use ulid::Ulid;

fn client_id(name: &str) -> String {
    format!("test-{name}-{}", Ulid::new())
}

fn deferred_options(id: &str) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_deferred_ack(true)
        .with_clean_start(false)
        .with_session_expiry_interval(3600)
        .with_receive_maximum(16)
        .with_keep_alive(Duration::from_secs(5))
}

fn qos2_subscribe() -> SubscribeOptions {
    SubscribeOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    }
}

fn qos2_publish() -> PublishOptions {
    PublishOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    }
}

async fn wait_until(cond: impl Fn() -> bool) -> bool {
    for _ in 0..200 {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

async fn publish_once(broker_address: &str, topic: &str, payload: &[u8]) {
    let publisher = MqttClient::new(client_id("def-pub"));
    publisher.connect(broker_address).await.unwrap();
    publisher
        .publish_with_options(topic, payload.to_vec(), qos2_publish())
        .await
        .unwrap();
    publisher.disconnect().await.ok();
}

#[tokio::test]
async fn deferred_ack_delivers_and_completes_qos2_handshake() {
    let broker = TestBroker::start().await;
    let subscriber = MqttClient::with_options(deferred_options(&client_id("def-basic")));
    subscriber.connect(broker.address()).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let tokens: Arc<Mutex<Vec<AckToken>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cb = Arc::clone(&received);
    let tokens_cb = Arc::clone(&tokens);
    subscriber
        .subscribe_with_ack("jobs/#", qos2_subscribe(), move |_publish, token| {
            received_cb.fetch_add(1, Ordering::SeqCst);
            tokens_cb.lock().unwrap().push(token);
        })
        .await
        .unwrap();

    publish_once(broker.address(), "jobs/build", b"hello").await;

    assert!(
        wait_until(|| received.load(Ordering::SeqCst) == 1).await,
        "message must be delivered exactly once"
    );

    let token = tokens.lock().unwrap().pop().expect("token delivered");
    token.ack();

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        received.load(Ordering::SeqCst),
        1,
        "no duplicate delivery after the handshake completes"
    );

    subscriber.disconnect().await.ok();
}

#[tokio::test]
async fn deferred_ack_resubscribes_after_session_lost() {
    let mut broker = TestBroker::start().await;
    let sub_id = client_id("def-resub");
    let opts = deferred_options(&sub_id);
    let subscriber = MqttClient::with_options(opts.clone());
    subscriber.connect(broker.address()).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_cb = Arc::clone(&received);
    subscriber
        .subscribe_with_ack("jobs/#", qos2_subscribe(), move |_publish, token| {
            received_cb.fetch_add(1, Ordering::SeqCst);
            token.ack();
        })
        .await
        .unwrap();

    publish_once(broker.address(), "jobs/a", b"before").await;
    assert!(
        wait_until(|| received.load(Ordering::SeqCst) == 1).await,
        "first message delivered before the session is lost"
    );

    broker.stop().await;
    broker.restart().await;
    if subscriber.is_connected().await {
        subscriber.disconnect().await.ok();
    }

    let result = subscriber
        .connect_with_options(broker.address(), opts)
        .await
        .unwrap();
    assert!(
        !result.session_present,
        "a restarted memory-backed broker must report no session"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;

    publish_once(broker.address(), "jobs/b", b"after").await;
    assert!(
        wait_until(|| received.load(Ordering::SeqCst) == 2).await,
        "the deferred-ack subscription must be re-established after a session_present=0 reconnect"
    );

    subscriber.disconnect().await.ok();
}

#[tokio::test]
async fn deferred_ack_exactly_once_across_transport_reconnect() {
    let broker = TestBroker::start().await;
    let sub_id = client_id("def-xport");
    let opts = deferred_options(&sub_id);
    let subscriber = MqttClient::with_options(opts.clone());
    subscriber.connect(broker.address()).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let tokens: Arc<Mutex<Vec<AckToken>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cb = Arc::clone(&received);
    let tokens_cb = Arc::clone(&tokens);
    subscriber
        .subscribe_with_ack("jobs/#", qos2_subscribe(), move |_publish, token| {
            received_cb.fetch_add(1, Ordering::SeqCst);
            tokens_cb.lock().unwrap().push(token);
        })
        .await
        .unwrap();

    publish_once(broker.address(), "jobs/x", b"withheld").await;
    assert!(
        wait_until(|| received.load(Ordering::SeqCst) == 1).await,
        "message delivered before the reconnect, token withheld (not acked)"
    );

    subscriber.disconnect().await.unwrap();
    let result = subscriber
        .connect_with_options(broker.address(), opts)
        .await
        .unwrap();
    assert!(
        result.session_present,
        "the broker kept running, so the resumed session must be present"
    );

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        received.load(Ordering::SeqCst),
        1,
        "a duplicate PUBLISH replayed on resume must not re-deliver the message"
    );

    let token = tokens.lock().unwrap().pop().expect("token still held");
    token.ack();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(received.load(Ordering::SeqCst), 1);

    subscriber.disconnect().await.ok();
}
