//! Sections 3.8–3.9 — SUBSCRIBE and SUBACK.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use crate::test_client::TestClient;
use mqtt5_protocol::types::{PublishOptions, QoS, RetainHandling, SubscribeOptions};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// `[MQTT-3.8.1-1]` SUBSCRIBE fixed header flags MUST be `0x02`.
/// A raw SUBSCRIBE with flags `0x00` (byte `0x80`) must cause disconnect.
#[conformance_test(
    ids = ["MQTT-3.8.1-1"],
    requires = ["transport.tcp"],
)]
async fn subscribe_invalid_flags_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("sub-flags");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_invalid_flags("test/topic", 0))
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.8.1-1] server must disconnect on SUBSCRIBE with invalid flags"
    );
}

/// `[MQTT-3.8.3-3]` SUBSCRIBE payload MUST contain at least one topic filter.
#[conformance_test(
    ids = ["MQTT-3.8.3-3"],
    requires = ["transport.tcp"],
)]
async fn subscribe_empty_payload_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("sub-empty");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_empty_payload(1))
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.8.3-3] server must disconnect on SUBSCRIBE with no topic filters"
    );
}

/// `[MQTT-3.8.3-4]` `NoLocal=1` on a shared subscription is a Protocol Error.
#[conformance_test(
    ids = ["MQTT-3.8.3-4"],
    requires = ["transport.tcp", "shared_subscription_available"],
)]
async fn subscribe_no_local_shared_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("sub-nolocal-share");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_shared_no_local(
        "workers", "tasks/+", 1, 1,
    ))
    .await
    .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.8.3-4] server must disconnect on NoLocal with shared subscription"
    );
}

/// `[MQTT-3.9.2-1]` SUBACK packet ID must match SUBSCRIBE packet ID.
#[conformance_test(
    ids = ["MQTT-3.9.2-1"],
    requires = ["transport.tcp"],
)]
async fn suback_packet_id_matches(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("suback-pid");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    let packet_id: u16 = 42;
    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/suback-pid",
        0,
        packet_id,
    ))
    .await
    .unwrap();

    let (ack_id, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("expected SUBACK from broker");

    assert_eq!(
        ack_id, packet_id,
        "[MQTT-3.9.2-1] SUBACK packet ID must match SUBSCRIBE packet ID"
    );
    assert_eq!(reason_codes.len(), 1, "SUBACK must contain one reason code");
}

/// `[MQTT-3.9.3-1]` SUBACK must contain one reason code per topic filter.
#[conformance_test(
    ids = ["MQTT-3.9.3-1"],
    requires = ["transport.tcp"],
)]
async fn suback_reason_codes_per_filter(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("suback-multi");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    let filters = [("test/a", 0u8), ("test/b", 1), ("test/c", 2)];
    raw.send_raw(&RawPacketBuilder::subscribe_multiple(&filters, 10))
        .await
        .unwrap();

    let (ack_id, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("expected SUBACK from broker");

    assert_eq!(ack_id, 10);
    assert_eq!(
        reason_codes.len(),
        3,
        "[MQTT-3.9.3-1] SUBACK must contain one reason code per topic filter"
    );
}

/// `[MQTT-3.9.3-3]` SUBACK grants the exact `QoS` requested on a default
/// broker (max `QoS` 2).
#[conformance_test(
    ids = ["MQTT-3.9.3-3"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn suback_grants_requested_qos(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("suback-qos");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    for qos in 0..=2u8 {
        let topic = format!("test/qos{qos}");
        let packet_id = u16::from(qos) + 1;
        raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
            &topic, qos, packet_id,
        ))
        .await
        .unwrap();

        let (ack_id, reason_codes) = raw
            .expect_suback(TIMEOUT)
            .await
            .expect("expected SUBACK from broker");

        assert_eq!(ack_id, packet_id);
        assert_eq!(
            reason_codes[0], qos,
            "SUBACK should grant QoS {qos}, got 0x{:02X}",
            reason_codes[0]
        );
    }
}

/// `[MQTT-3.8.4-3]` Subscribing twice to the same topic with different `QoS`
/// replaces the existing subscription. Only one copy of a published message
/// is delivered.
#[conformance_test(
    ids = ["MQTT-3.8.4-3"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn subscribe_replaces_existing(sut: SutHandle) {
    let mut raw_sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub-replace");
    raw_sub.connect_and_establish(&sub_id, TIMEOUT).await;

    raw_sub
        .send_raw(&RawPacketBuilder::subscribe_with_packet_id(
            "test/replace",
            0,
            1,
        ))
        .await
        .unwrap();
    let (_, rc1) = raw_sub.expect_suback(TIMEOUT).await.expect("SUBACK 1");
    assert_eq!(rc1[0], 0x00, "first subscribe should grant QoS 0");

    raw_sub
        .send_raw(&RawPacketBuilder::subscribe_with_packet_id(
            "test/replace",
            1,
            2,
        ))
        .await
        .unwrap();
    let (_, rc2) = raw_sub.expect_suback(TIMEOUT).await.expect("SUBACK 2");
    assert_eq!(rc2[0], 0x01, "second subscribe should grant QoS 1");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "sub-replace-pub")
        .await
        .unwrap();
    publisher.publish("test/replace", b"once").await.unwrap();

    let first = raw_sub.expect_publish(TIMEOUT).await;
    assert!(first.is_some(), "subscriber should receive the message");

    let duplicate = raw_sub.expect_publish(Duration::from_millis(500)).await;
    assert!(
        duplicate.is_none(),
        "[MQTT-3.8.4-3] subscriber should receive only one copy (replacement, not duplicate subscription)"
    );
}

/// `[MQTT-3.8.3-5]` Reserved bits in the subscribe options byte MUST be zero.
#[conformance_test(
    ids = ["MQTT-3.8.3-5"],
    requires = ["transport.tcp"],
)]
async fn subscribe_reserved_option_bits_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("sub-reserved-bits");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_options(
        "test/reserved-bits",
        0xC0,
        1,
    ))
    .await
    .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.8.3-5] server must disconnect on SUBSCRIBE with reserved option bits set"
    );
}

/// `[MQTT-3.8.3-1]` Topic filter in SUBSCRIBE must be valid UTF-8.
#[conformance_test(
    ids = ["MQTT-3.8.3-1"],
    requires = ["transport.tcp"],
)]
async fn subscribe_invalid_utf8_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("sub-bad-utf8");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_invalid_utf8(1))
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.8.3-1] server must disconnect on SUBSCRIBE with invalid UTF-8 topic filter"
    );
}

/// `[MQTT-3.8.4-4]` When Retain Handling is 0 (`SendAtSubscribe`), retained
/// messages are sent on every subscribe, including re-subscribes.
#[conformance_test(
    ids = ["MQTT-3.8.4-4"],
    requires = ["transport.tcp", "retain_available"],
)]
async fn retain_handling_zero_sends_on_resubscribe(sut: SutHandle) {
    let publisher = TestClient::connect_with_prefix(&sut, "rh0-pub")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::AtMostOnce,
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options("test/rh0/topic", b"retained-payload", pub_opts)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let subscriber = TestClient::connect_with_prefix(&sut, "rh0-sub")
        .await
        .unwrap();
    let sub_opts = SubscribeOptions {
        qos: QoS::AtMostOnce,
        retain_handling: RetainHandling::SendAtSubscribe,
        ..Default::default()
    };
    let subscription = subscriber
        .subscribe("test/rh0/topic", sub_opts.clone())
        .await
        .unwrap();

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "first subscribe with RetainHandling=0 must deliver retained message"
    );
    let msgs = subscription.snapshot();
    assert_eq!(msgs[0].payload, b"retained-payload");

    let subscription2 = subscriber
        .subscribe("test/rh0/topic", sub_opts)
        .await
        .unwrap();

    assert!(
        subscription2.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.8.4-4] re-subscribe with RetainHandling=0 must deliver retained message again"
    );
    let msgs2 = subscription2.snapshot();
    assert_eq!(msgs2[0].payload, b"retained-payload");
}

/// `[MQTT-3.8.4-8]` The delivered `QoS` is the minimum of the published `QoS`
/// and the subscription's granted `QoS`. Subscribe at `QoS` 0, publish at
/// `QoS` 1 → delivered at `QoS` 0.
#[conformance_test(
    ids = ["MQTT-3.8.4-8"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn delivered_qos_is_minimum_sub0_pub1(sut: SutHandle) {
    let tag = unique_client_id("minqos01");
    let topic = format!("minqos/{tag}");

    let mut raw_sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub-mq01");
    raw_sub.connect_and_establish(&sub_id, TIMEOUT).await;

    raw_sub
        .send_raw(&RawPacketBuilder::subscribe_with_packet_id(&topic, 0, 1))
        .await
        .unwrap();
    let (_, rc) = raw_sub.expect_suback(TIMEOUT).await.expect("SUBACK");
    assert_eq!(rc[0], 0x00, "granted QoS should be 0");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut raw_publisher = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("pub-mq01");
    raw_publisher.connect_and_establish(&pub_id, TIMEOUT).await;

    raw_publisher
        .send_raw(&RawPacketBuilder::publish_qos1(&topic, b"hello", 1))
        .await
        .unwrap();

    let msg = raw_sub.expect_publish(TIMEOUT).await;
    assert!(msg.is_some(), "subscriber should receive the message");
    let (qos, _, payload) = msg.unwrap();
    assert_eq!(payload, b"hello");
    assert_eq!(
        qos, 0,
        "[MQTT-3.8.4-8] delivered QoS must be min(pub=1, sub=0) = 0"
    );
}

/// `[MQTT-3.8.4-8]` Subscribe at `QoS` 1, publish at `QoS` 2 → delivered at
/// `QoS` 1.
#[conformance_test(
    ids = ["MQTT-3.8.4-8"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn delivered_qos_is_minimum_sub1_pub2(sut: SutHandle) {
    let tag = unique_client_id("minqos12");
    let topic = format!("minqos/{tag}");

    let mut raw_sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub-mq12");
    raw_sub.connect_and_establish(&sub_id, TIMEOUT).await;

    raw_sub
        .send_raw(&RawPacketBuilder::subscribe_with_packet_id(&topic, 1, 1))
        .await
        .unwrap();
    let (_, rc) = raw_sub.expect_suback(TIMEOUT).await.expect("SUBACK");
    assert_eq!(rc[0], 0x01, "granted QoS should be 1");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub-mq12")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"world", pub_opts)
        .await
        .unwrap();

    let msg = raw_sub.expect_publish(TIMEOUT).await;
    assert!(msg.is_some(), "subscriber should receive the message");
    let (qos, _, payload) = msg.unwrap();
    assert_eq!(payload, b"world");
    assert_eq!(
        qos, 1,
        "[MQTT-3.8.4-8] delivered QoS must be min(pub=2, sub=1) = 1"
    );
}
