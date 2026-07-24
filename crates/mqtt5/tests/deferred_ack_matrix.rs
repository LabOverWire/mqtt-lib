#![cfg(feature = "broker")]
#![allow(clippy::large_futures)]

mod common;

use common::TestBroker;
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::time::Duration;
use mqtt5::{AckToken, ConnectOptions, MqttClient, PublishOptions, QoS, SubscribeOptions};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use ulid::Ulid;

fn client_id(name: &str) -> String {
    format!("test-{name}-{}", Ulid::new())
}

fn deferred_options(id: &str, receive_maximum: u16) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_deferred_ack(true)
        .with_clean_start(false)
        .with_session_expiry_interval(3600)
        .with_receive_maximum(receive_maximum)
        .with_keep_alive(Duration::from_secs(5))
}

fn subscribe(qos: QoS) -> SubscribeOptions {
    SubscribeOptions {
        qos,
        ..Default::default()
    }
}

fn publish(qos: QoS) -> PublishOptions {
    PublishOptions {
        qos,
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

async fn publish_n(broker_address: &str, topic: &str, qos: QoS, count: u32) {
    let publisher = MqttClient::new(client_id("def-pub"));
    publisher.connect(broker_address).await.unwrap();
    publish_n_with(&publisher, topic, qos, count).await;
    publisher.disconnect().await.ok();
}

async fn publish_n_with(publisher: &MqttClient, topic: &str, qos: QoS, count: u32) {
    for i in 0..count {
        publisher
            .publish_with_options(topic, format!("msg-{i}").into_bytes(), publish(qos))
            .await
            .unwrap();
    }
}

struct Recorder {
    count: Arc<AtomicU32>,
    tokens: Arc<Mutex<Vec<AckToken>>>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            count: Arc::new(AtomicU32::new(0)),
            tokens: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn received(&self) -> u32 {
        self.count.load(Ordering::SeqCst)
    }

    fn pop_token(&self) -> AckToken {
        self.tokens.lock().unwrap().pop().expect("a token was held")
    }
}

async fn subscribe_collecting(client: &MqttClient, filter: &str, qos: QoS, rec: &Recorder) {
    let count = Arc::clone(&rec.count);
    let tokens = Arc::clone(&rec.tokens);
    client
        .subscribe_with_ack(filter, subscribe(qos), move |_publish, token| {
            count.fetch_add(1, Ordering::SeqCst);
            tokens.lock().unwrap().push(token);
        })
        .await
        .unwrap();
}

// Q1 (the untested gap): QoS1 deferred ack. Holding the token withholds the PUBACK, which
// keeps the Receive-Maximum slot full and throttles the broker; acking frees the slot.
#[tokio::test]
async fn qos1_deferred_puback_withheld_throttles_then_ack_frees_slot() {
    let broker = TestBroker::start().await;
    let subscriber = MqttClient::with_options(deferred_options(&client_id("q1"), 1));
    subscriber.connect(broker.address()).await.unwrap();

    let rec = Recorder::new();
    subscribe_collecting(&subscriber, "jobs/#", QoS::AtLeastOnce, &rec).await;

    publish_n(broker.address(), "jobs/a", QoS::AtLeastOnce, 2).await;

    assert!(
        wait_until(|| rec.received() == 1).await,
        "first QoS1 message delivered"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        rec.received(),
        1,
        "the second message is withheld while the PUBACK is deferred (window full)"
    );

    rec.pop_token().ack();

    assert!(
        wait_until(|| rec.received() == 2).await,
        "acking the first frees the slot and the second QoS1 message flows"
    );

    subscriber.disconnect().await.ok();
}

// Q2 + B1/B2: QoS2 deferred PUBREC withheld throttles the window; ack frees it.
#[tokio::test]
async fn qos2_deferred_pubrec_withheld_throttles_then_ack_frees_slot() {
    let broker = TestBroker::start().await;
    let subscriber = MqttClient::with_options(deferred_options(&client_id("q2"), 1));
    subscriber.connect(broker.address()).await.unwrap();

    let rec = Recorder::new();
    subscribe_collecting(&subscriber, "jobs/#", QoS::ExactlyOnce, &rec).await;

    publish_n(broker.address(), "jobs/a", QoS::ExactlyOnce, 2).await;

    assert!(
        wait_until(|| rec.received() == 1).await,
        "first QoS2 message delivered"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        rec.received(),
        1,
        "the second message is withheld while the PUBREC is deferred (window full)"
    );

    rec.pop_token().ack();

    assert!(
        wait_until(|| rec.received() == 2).await,
        "acking completes the handshake, frees the slot, and the second QoS2 message flows"
    );

    subscriber.disconnect().await.ok();
}

// Q3: rejecting a QoS2 message is terminal and frees the slot (next message flows).
#[tokio::test]
async fn qos2_reject_frees_the_slot() {
    let broker = TestBroker::start().await;
    let subscriber = MqttClient::with_options(deferred_options(&client_id("q3"), 1));
    subscriber.connect(broker.address()).await.unwrap();

    let rec = Recorder::new();
    subscribe_collecting(&subscriber, "jobs/#", QoS::ExactlyOnce, &rec).await;

    publish_n(broker.address(), "jobs/a", QoS::ExactlyOnce, 2).await;
    assert!(wait_until(|| rec.received() == 1).await, "first delivered");

    rec.pop_token().reject(ReasonCode::UnspecifiedError);

    assert!(
        wait_until(|| rec.received() == 2).await,
        "rejecting frees the slot and the next message flows"
    );

    subscriber.disconnect().await.ok();
}

// Q5 + obligation 7: dropping an armed token auto-acks, freeing the slot (no wedge).
#[tokio::test]
async fn qos2_dropped_token_auto_acks_and_frees_the_slot() {
    let broker = TestBroker::start().await;
    let subscriber = MqttClient::with_options(deferred_options(&client_id("q5"), 1));
    subscriber.connect(broker.address()).await.unwrap();

    let rec = Recorder::new();
    subscribe_collecting(&subscriber, "jobs/#", QoS::ExactlyOnce, &rec).await;

    publish_n(broker.address(), "jobs/a", QoS::ExactlyOnce, 2).await;
    assert!(wait_until(|| rec.received() == 1).await, "first delivered");

    drop(rec.pop_token());

    assert!(
        wait_until(|| rec.received() == 2).await,
        "dropping the token auto-acks, frees the slot, and the next message flows (no wedge)"
    );

    subscriber.disconnect().await.ok();
}

// R1 (obligation 4): with the window saturated by held tokens, the reader is not blocked —
// the control plane still services a fresh SUBSCRIBE and the connection stays alive.
#[tokio::test]
async fn saturated_window_does_not_block_the_control_plane() {
    let broker = TestBroker::start().await;
    let subscriber = MqttClient::with_options(deferred_options(&client_id("r1"), 2));
    subscriber.connect(broker.address()).await.unwrap();

    let rec = Recorder::new();
    subscribe_collecting(&subscriber, "jobs/#", QoS::ExactlyOnce, &rec).await;

    publish_n(broker.address(), "jobs/a", QoS::ExactlyOnce, 4).await;
    assert!(
        wait_until(|| rec.received() == 2).await,
        "the window fills at receive_maximum=2 with tokens held"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rec.received(), 2, "no further delivery while saturated");

    let ping = MqttClient::with_options(deferred_options(&client_id("r1-probe"), 2));
    ping.connect(broker.address()).await.unwrap();
    ping.subscribe_with_ack("other/#", subscribe(QoS::AtLeastOnce), |_p, _t| {})
        .await
        .expect("a SUBSCRIBE must still succeed while another client's window is saturated");
    assert!(
        subscriber.is_connected().await,
        "the saturated subscriber stays connected (reader not blocked)"
    );

    ping.disconnect().await.ok();
    subscriber.disconnect().await.ok();
}

// RC2 (_crash regime): a fresh client process resumes the session on a still-running broker;
// the unacked QoS2 message is re-delivered (at-least-once processing) with no wedge.
#[tokio::test]
async fn crash_regime_fresh_client_resumes_and_redelivers() {
    let broker = TestBroker::start().await;
    let id = client_id("rc2");

    let first = MqttClient::with_options(deferred_options(&id, 8));
    first.connect(broker.address()).await.unwrap();
    let rec1 = Recorder::new();
    subscribe_collecting(&first, "jobs/#", QoS::ExactlyOnce, &rec1).await;

    publish_n(broker.address(), "jobs/a", QoS::ExactlyOnce, 1).await;
    assert!(
        wait_until(|| rec1.received() == 1).await,
        "the first client receives the message"
    );

    // Simulate a crash: drop the client without acking and without a graceful DISCONNECT.
    drop(rec1);
    drop(first);
    tokio::time::sleep(Duration::from_millis(200)).await;

    // A fresh client with the same id resumes the persistent session.
    let second = MqttClient::with_options(deferred_options(&id, 8));
    let result = second
        .connect_with_options(broker.address(), deferred_options(&id, 8))
        .await
        .unwrap();
    assert!(
        result.session_present,
        "the still-running broker resumes the persistent session"
    );

    let rec2 = Recorder::new();
    subscribe_collecting(&second, "jobs/#", QoS::ExactlyOnce, &rec2).await;

    assert!(
        wait_until(|| rec2.received() == 1).await,
        "the unacked message is re-delivered to the resumed session (at-least-once, no wedge)"
    );
    rec2.pop_token().ack();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rec2.received(), 1, "no further re-delivery after the ack");

    second.disconnect().await.ok();
}

async fn connect_tls(
    client: &MqttClient,
    broker: &TestBroker,
    options: ConnectOptions,
) -> mqtt5::ConnectResult {
    use mqtt5::transport::tls::TlsConfig;
    let addr: std::net::SocketAddr = broker
        .address()
        .strip_prefix("mqtts://")
        .expect("tls broker address")
        .parse()
        .expect("socket addr");
    let mut tls = TlsConfig::new(addr, "localhost").with_verify_server_cert(false);
    tls.load_ca_cert_pem("../../test_certs/ca.pem")
        .expect("load test CA cert");
    client
        .connect_with_tls_and_options(tls, options)
        .await
        .expect("tls connect")
}

// TLS smoke: the deferred QoS2 path (deliver, backpressure, ack) works under encryption.
#[tokio::test]
async fn tls_deferred_qos2_delivers_backpressures_and_acks() {
    let broker = TestBroker::start_with_tls().await;

    let sub_opts = deferred_options(&client_id("tls-sub"), 1);
    let subscriber = MqttClient::with_options(sub_opts.clone());
    connect_tls(&subscriber, &broker, sub_opts).await;

    let rec = Recorder::new();
    subscribe_collecting(&subscriber, "jobs/#", QoS::ExactlyOnce, &rec).await;

    let publisher = MqttClient::with_options(ConnectOptions::new(client_id("tls-pub")));
    connect_tls(
        &publisher,
        &broker,
        ConnectOptions::new(client_id("tls-pub")),
    )
    .await;
    publish_n_with(&publisher, "jobs/a", QoS::ExactlyOnce, 2).await;

    assert!(
        wait_until(|| rec.received() == 1).await,
        "first message delivered over TLS"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        rec.received(),
        1,
        "second withheld while the window is full"
    );

    rec.pop_token().ack();
    assert!(
        wait_until(|| rec.received() == 2).await,
        "acking over TLS frees the slot and the second message flows"
    );

    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
}

// WebSocket smoke: the deferred QoS2 path works over the WebSocket framing layer.
#[cfg(feature = "transport-websocket")]
#[tokio::test]
async fn websocket_deferred_qos2_delivers_backpressures_and_acks() {
    let broker = TestBroker::start_with_websocket().await;

    let sub_opts = deferred_options(&client_id("ws-sub"), 1);
    let subscriber = MqttClient::with_options(sub_opts.clone());
    subscriber
        .connect_with_options(broker.address(), sub_opts)
        .await
        .unwrap();

    let rec = Recorder::new();
    subscribe_collecting(&subscriber, "jobs/#", QoS::ExactlyOnce, &rec).await;

    let publisher = MqttClient::new(client_id("ws-pub"));
    publisher.connect(broker.address()).await.unwrap();
    publish_n_with(&publisher, "jobs/a", QoS::ExactlyOnce, 2).await;

    assert!(
        wait_until(|| rec.received() == 1).await,
        "first message delivered over WebSocket"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        rec.received(),
        1,
        "second withheld while the window is full"
    );

    rec.pop_token().ack();
    assert!(
        wait_until(|| rec.received() == 2).await,
        "acking over WebSocket frees the slot and the second message flows"
    );

    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
}
