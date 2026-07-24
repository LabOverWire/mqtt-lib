#![cfg(all(feature = "broker", feature = "transport-quic"))]
#![allow(clippy::large_futures)]

//! Deferred acknowledgement over MQTT-over-QUIC (`MQoQ`).
//!
//! These run in the `DataPerTopic` stream strategy, so an inbound PUBLISH arrives on a per-topic
//! data flow while the deferred PUBREC is written on the shared (control-stream) writer. The tests
//! prove the broker still correlates that acknowledgement by packet id and completes the `QoS` 2
//! handshake: delivery, Receive-Maximum backpressure, and both `ack()` and `reject()` behave as
//! they do over TCP.

use mqtt5::broker::config::{BrokerConfig, QuicConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::time::Duration;
use mqtt5::transport::StreamStrategy;
use mqtt5::{AckToken, ConnectOptions, MqttClient, PublishOptions, QoS, SubscribeOptions};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use ulid::Ulid;

fn cid(prefix: &str) -> String {
    format!("{prefix}-{}", Ulid::new())
}

async fn start_quic_broker() -> (mqtt5::broker::BrokerShutdownHandle, SocketAddr) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cert_dir = manifest_dir.join("../../test_certs");
    let quic_bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_quic(
            QuicConfig::new(cert_dir.join("server.pem"), cert_dir.join("server.key"))
                .with_bind_address(quic_bind),
        );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let quic_addr = broker.quic_local_addr().expect("QUIC endpoint bound");
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    let _ = ready.wait_for(|&v| v).await;
    (shutdown, quic_addr)
}

fn deferred_options(id: &str, receive_maximum: u16) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_deferred_ack(true)
        .with_clean_start(false)
        .with_session_expiry_interval(3600)
        .with_receive_maximum(receive_maximum)
        .with_keep_alive(Duration::from_secs(5))
}

fn qos2_sub() -> SubscribeOptions {
    SubscribeOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    }
}

fn qos2_pub() -> PublishOptions {
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

struct QuicFixture {
    shutdown: mqtt5::broker::BrokerShutdownHandle,
    subscriber: MqttClient,
    publisher: MqttClient,
    received: Arc<AtomicU32>,
    tokens: Arc<Mutex<Vec<AckToken>>>,
}

async fn setup(topic: &str) -> QuicFixture {
    let (shutdown, quic_addr) = start_quic_broker().await;
    let url = format!("quic://{quic_addr}");

    let sub_opts = deferred_options(&cid("quic-def-sub"), 1);
    let subscriber = MqttClient::with_options(sub_opts.clone());
    subscriber.set_insecure_tls(true).await;
    subscriber
        .set_quic_stream_strategy(StreamStrategy::DataPerTopic)
        .await;
    subscriber
        .connect_with_options(&url, sub_opts)
        .await
        .expect("subscriber connects over QUIC");

    let received = Arc::new(AtomicU32::new(0));
    let tokens = Arc::new(Mutex::new(Vec::new()));
    let rc = Arc::clone(&received);
    let tc = Arc::clone(&tokens);
    subscriber
        .subscribe_with_ack(topic, qos2_sub(), move |_p, token| {
            rc.fetch_add(1, Ordering::SeqCst);
            tc.lock().unwrap().push(token);
        })
        .await
        .expect("subscribe_with_ack over QUIC");

    let publisher = MqttClient::new(cid("quic-def-pub"));
    publisher.set_insecure_tls(true).await;
    publisher
        .set_quic_stream_strategy(StreamStrategy::DataPerTopic)
        .await;
    publisher.connect(&url).await.expect("publisher connects");

    QuicFixture {
        shutdown,
        subscriber,
        publisher,
        received,
        tokens,
    }
}

async fn publish_two(fixture: &QuicFixture, topic: &str) {
    for i in 0..2 {
        fixture
            .publisher
            .publish_with_options(topic, format!("m{i}").into_bytes(), qos2_pub())
            .await
            .expect("publish over QUIC");
    }
}

#[tokio::test]
async fn deferred_qos2_over_quic_delivers_backpressures_and_acks() {
    let topic = "jobs/a";
    let fixture = setup(topic).await;
    publish_two(&fixture, topic).await;

    assert!(
        wait_until(|| fixture.received.load(Ordering::SeqCst) == 1).await,
        "first QoS2 message delivered over QUIC"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        fixture.received.load(Ordering::SeqCst),
        1,
        "second withheld while the deferred PUBREC keeps the window full"
    );

    fixture.tokens.lock().unwrap().pop().unwrap().ack();

    assert!(
        wait_until(|| fixture.received.load(Ordering::SeqCst) == 2).await,
        "acking over QUIC completes the handshake, frees the slot, and the second message flows"
    );

    fixture.publisher.disconnect().await.ok();
    fixture.subscriber.disconnect().await.ok();
    fixture.shutdown.shutdown();
}

#[tokio::test]
async fn deferred_qos2_over_quic_reject_frees_the_slot() {
    let topic = "jobs/b";
    let fixture = setup(topic).await;
    publish_two(&fixture, topic).await;

    assert!(
        wait_until(|| fixture.received.load(Ordering::SeqCst) == 1).await,
        "first QoS2 message delivered over QUIC"
    );

    fixture
        .tokens
        .lock()
        .unwrap()
        .pop()
        .unwrap()
        .reject(ReasonCode::UnspecifiedError);

    assert!(
        wait_until(|| fixture.received.load(Ordering::SeqCst) == 2).await,
        "rejecting over QUIC is terminal, frees the slot, and the next message flows"
    );

    fixture.publisher.disconnect().await.ok();
    fixture.subscriber.disconnect().await.ok();
    fixture.shutdown.shutdown();
}
