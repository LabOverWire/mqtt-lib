//! Broker behaviour that the client-side deferred-acknowledgement feature relies on.
//!
//! Deferred ack works by withholding the PUBREC/PUBACK for an inbound message until the application
//! has processed it. That only provides backpressure (rather than silent unbounded buffering) if the
//! broker keeps the Receive-Maximum slot occupied for the whole time the acknowledgement is withheld
//! — for `QoS` 2, across the entire PUBREC/PUBREL/PUBCOMP handshake, not just until PUBREC. These
//! tests drive a raw subscriber that defers its `QoS` 2 acknowledgement and assert the broker
//! behaves accordingly. They are broker-agnostic, so they run against the in-process fixture, the
//! external `mqttv5` broker, and any third-party broker with `QoS` 2 support.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use crate::test_client::TestClient;
use mqtt5_protocol::types::{PublishOptions, QoS};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(600);

async fn publish_qos2(sut: &SutHandle, topic: &str, payloads: &[&[u8]]) {
    let publisher = TestClient::connect_with_prefix(sut, "def-pub")
        .await
        .unwrap();
    for payload in payloads {
        publisher
            .publish_with_options(
                topic,
                payload,
                PublishOptions {
                    qos: QoS::ExactlyOnce,
                    ..PublishOptions::default()
                },
            )
            .await
            .unwrap();
    }
    publisher.disconnect().await.ok();
}

/// `[MQTT-4.9.0-1]` `[MQTT-4.9.0-2]` A withheld `QoS` 2 PUBREC keeps the Receive-Maximum slot
/// occupied: the broker delivers only `receive_maximum` messages and releases the next one only
/// after the full PUBREC/PUBREL/PUBCOMP handshake completes — the guarantee deferred ack depends on.
#[conformance_test(
    ids = ["MQTT-4.9.0-1", "MQTT-4.9.0-2"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn deferred_qos2_slot_held_until_pubcomp(sut: SutHandle) {
    let topic = format!("deferred-q2/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("def-sub");
    sub.send_raw(&RawPacketBuilder::connect_with_receive_maximum(&sub_id, 1))
        .await
        .unwrap();
    let (_, reason) = sub.expect_connack(TIMEOUT).await.expect("CONNACK");
    assert_eq!(reason, 0x00, "CONNACK must be Success");

    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 2))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await.expect("SUBACK");

    publish_qos2(&sut, &topic, &[b"first", b"second"]).await;

    let (_, first_pid, qos, _t, _p) = sub
        .expect_publish_with_id(TIMEOUT)
        .await
        .expect("broker delivers the first QoS 2 PUBLISH");
    assert_eq!(qos, 2, "delivered message must be QoS 2");

    assert!(
        sub.expect_publish_with_id(QUIET).await.is_none(),
        "[MQTT-4.9.0-2] the second message must be withheld while the PUBREC is deferred (quota=1)"
    );

    sub.send_raw(&RawPacketBuilder::pubrec(first_pid))
        .await
        .unwrap();
    let (_, rel_pid, _) = sub
        .expect_pubrel_raw(TIMEOUT)
        .await
        .expect("broker answers PUBREC with PUBREL");
    assert_eq!(rel_pid, first_pid, "PUBREL must carry the same packet id");
    sub.send_raw(&RawPacketBuilder::pubcomp(first_pid))
        .await
        .unwrap();

    let (_, second_pid, qos2, _t, _p) = sub.expect_publish_with_id(TIMEOUT).await.expect(
        "[MQTT-4.9.0-1] the second message flows once the handshake completes and frees the slot",
    );
    assert_eq!(qos2, 2, "second delivered message must be QoS 2");
    assert_ne!(
        second_pid, 0,
        "the second delivery must carry a valid non-zero packet id (reuse of a freed id is allowed)"
    );

    sub.send_raw(&RawPacketBuilder::pubrec(second_pid))
        .await
        .unwrap();
    let _ = sub.expect_pubrel_raw(TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::pubcomp(second_pid))
        .await
        .unwrap();
}

/// `[MQTT-4.9.0-3]` With the `QoS` 2 quota fully consumed by a withheld PUBREC, the broker must
/// still service control packets — PINGREQ and SUBSCRIBE — so a deferring client's control plane is
/// never blocked.
#[conformance_test(
    ids = ["MQTT-4.9.0-3"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn deferred_qos2_zero_quota_still_serves_control_plane(sut: SutHandle) {
    let topic = format!("deferred-q2-zero/{}", unique_client_id("t"));
    let topic2 = format!("deferred-q2-zero2/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("def-sub0");
    sub.send_raw(&RawPacketBuilder::connect_with_receive_maximum(&sub_id, 1))
        .await
        .unwrap();
    let (_, reason) = sub.expect_connack(TIMEOUT).await.expect("CONNACK");
    assert_eq!(reason, 0x00, "CONNACK must be Success");

    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 2))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await.expect("SUBACK");

    publish_qos2(&sut, &topic, &[b"fill", b"queued"]).await;

    let (_, _pid, qos, _t, _p) = sub
        .expect_publish_with_id(TIMEOUT)
        .await
        .expect("broker delivers the first QoS 2 PUBLISH");
    assert_eq!(qos, 2, "delivered message must be QoS 2");

    sub.send_raw(&RawPacketBuilder::pingreq()).await.unwrap();
    assert!(
        sub.expect_pingresp(TIMEOUT).await,
        "[MQTT-4.9.0-3] PINGREQ must be answered while the QoS 2 quota is fully withheld"
    );

    sub.send_raw(&RawPacketBuilder::subscribe_with_packet_id(&topic2, 0, 7))
        .await
        .unwrap();
    assert!(
        sub.expect_suback(TIMEOUT).await.is_some(),
        "[MQTT-4.9.0-3] SUBSCRIBE must be processed while the QoS 2 quota is fully withheld"
    );
}
