#![allow(clippy::cast_possible_truncation)]

mod common;

use common::{
    connected_client, unique_client_id, ConformanceBroker, MessageCollector, RawMqttClient,
    RawPacketBuilder,
};
use mqtt5::{QoS, SubscribeOptions};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Group 1: PUBACK — Section 3.4
// ---------------------------------------------------------------------------

/// Server MUST send PUBACK in response to a `QoS` 1 PUBLISH, containing
/// the matching Packet Identifier and a valid reason code.
#[tokio::test]
async fn puback_correct_packet_id_and_reason() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("puback-pid");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    let packet_id: u16 = 42;
    raw.send_raw(&RawPacketBuilder::publish_qos1(
        "test/puback",
        b"hello",
        packet_id,
    ))
    .await
    .unwrap();

    let (ack_id, reason) = raw
        .expect_puback(TIMEOUT)
        .await
        .expect("expected PUBACK from broker");

    assert_eq!(
        ack_id, packet_id,
        "PUBACK packet ID must match PUBLISH packet ID"
    );
    assert_eq!(reason, 0x00, "PUBACK reason code should be Success (0x00)");
}

/// `QoS` 1 PUBLISH results in message delivery to subscriber.
#[tokio::test]
async fn puback_message_delivered_on_qos1() {
    let broker = ConformanceBroker::start().await;
    let subscriber = connected_client("puback-sub", &broker).await;
    let collector = MessageCollector::new();
    let opts = SubscribeOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    subscriber
        .subscribe_with_options("test/qos1-deliver", opts, collector.callback())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("puback-pub", &broker).await;
    publisher
        .publish("test/qos1-deliver", b"qos1-payload".to_vec())
        .await
        .unwrap();

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "subscriber should receive QoS 1 message"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].payload, b"qos1-payload");
}

// ---------------------------------------------------------------------------
// Group 2: PUBREC — Section 3.5
// ---------------------------------------------------------------------------

/// Server MUST send PUBREC in response to a `QoS` 2 PUBLISH, containing
/// the matching Packet Identifier and a valid reason code.
#[tokio::test]
async fn pubrec_correct_packet_id_and_reason() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("pubrec-pid");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    let packet_id: u16 = 100;
    raw.send_raw(&RawPacketBuilder::publish_qos2(
        "test/pubrec",
        b"hello",
        packet_id,
    ))
    .await
    .unwrap();

    let (rec_id, reason) = raw
        .expect_pubrec(TIMEOUT)
        .await
        .expect("expected PUBREC from broker");

    assert_eq!(
        rec_id, packet_id,
        "PUBREC packet ID must match PUBLISH packet ID"
    );
    assert_eq!(reason, 0x00, "PUBREC reason code should be Success (0x00)");
}

/// `QoS` 2 message MUST NOT be delivered to subscribers until PUBREL is received.
///
/// Sends `QoS` 2 PUBLISH → gets PUBREC, but does NOT send PUBREL.
/// Verifies subscriber does not receive the message.
#[tokio::test]
async fn pubrec_no_delivery_before_pubrel() {
    let broker = ConformanceBroker::start().await;

    let subscriber = connected_client("pubrec-nosub", &broker).await;
    let collector = MessageCollector::new();
    let opts = SubscribeOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    subscriber
        .subscribe_with_options("test/nodelay", opts, collector.callback())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("pubrec-raw");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_qos2(
        "test/nodelay",
        b"held-back",
        1,
    ))
    .await
    .unwrap();

    let _ = raw
        .expect_pubrec(TIMEOUT)
        .await
        .expect("expected PUBREC from broker");

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        collector.count().await,
        0,
        "message must NOT be delivered before PUBREL"
    );
}

// ---------------------------------------------------------------------------
// Group 3: PUBREL — Section 3.6
// ---------------------------------------------------------------------------

/// `[MQTT-3.6.1-1]` PUBREL fixed header flags MUST be `0x02`. The Server MUST
/// treat any other value as malformed and close the Network Connection.
#[tokio::test]
async fn pubrel_invalid_flags_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("pubrel-flags");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_qos2(
        "test/pubrel-flags",
        b"payload",
        1,
    ))
    .await
    .unwrap();

    let _ = raw.expect_pubrec(TIMEOUT).await.expect("expected PUBREC");

    raw.send_raw(&RawPacketBuilder::pubrel_invalid_flags(1))
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.6.1-1] server must disconnect on PUBREL with invalid flags"
    );
}

/// PUBREL for an unknown Packet Identifier MUST result in PUBCOMP with
/// reason code `PacketIdentifierNotFound` (0x92).
#[tokio::test]
async fn pubrel_unknown_packet_id() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("pubrel-unk");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::pubrel(999)).await.unwrap();

    let (comp_id, reason) = raw
        .expect_pubcomp(TIMEOUT)
        .await
        .expect("expected PUBCOMP from broker");

    assert_eq!(comp_id, 999, "PUBCOMP packet ID must match PUBREL");
    assert_eq!(
        reason, 0x92,
        "PUBCOMP reason code should be PacketIdentifierNotFound (0x92)"
    );
}

// ---------------------------------------------------------------------------
// Group 4: PUBCOMP — Section 3.7
// ---------------------------------------------------------------------------

/// Full inbound `QoS` 2 flow: PUBLISH → PUBREC → PUBREL → PUBCOMP.
/// Verifies PUBCOMP has matching packet ID and reason=Success.
#[tokio::test]
async fn pubcomp_correct_packet_id_and_reason() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("pubcomp-pid");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    let packet_id: u16 = 77;
    raw.send_raw(&RawPacketBuilder::publish_qos2(
        "test/pubcomp",
        b"qos2-data",
        packet_id,
    ))
    .await
    .unwrap();

    let (rec_id, _) = raw.expect_pubrec(TIMEOUT).await.expect("expected PUBREC");
    assert_eq!(rec_id, packet_id);

    raw.send_raw(&RawPacketBuilder::pubrel(packet_id))
        .await
        .unwrap();

    let (comp_id, reason) = raw
        .expect_pubcomp(TIMEOUT)
        .await
        .expect("expected PUBCOMP from broker");

    assert_eq!(
        comp_id, packet_id,
        "PUBCOMP packet ID must match PUBLISH/PUBREL"
    );
    assert_eq!(reason, 0x00, "PUBCOMP reason code should be Success (0x00)");
}

/// Full `QoS` 2 exchange delivers the message to a subscriber.
#[tokio::test]
async fn pubcomp_message_delivered_after_exchange() {
    let broker = ConformanceBroker::start().await;

    let subscriber = connected_client("pubcomp-sub", &broker).await;
    let collector = MessageCollector::new();
    let opts = SubscribeOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    subscriber
        .subscribe_with_options("test/qos2-deliver", opts, collector.callback())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("pubcomp-raw");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_qos2(
        "test/qos2-deliver",
        b"qos2-payload",
        1,
    ))
    .await
    .unwrap();

    let _ = raw.expect_pubrec(TIMEOUT).await.expect("expected PUBREC");

    raw.send_raw(&RawPacketBuilder::pubrel(1)).await.unwrap();

    let _ = raw.expect_pubcomp(TIMEOUT).await.expect("expected PUBCOMP");

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "subscriber should receive message after full QoS 2 exchange"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].payload, b"qos2-payload");
}

// ---------------------------------------------------------------------------
// Group 5: Outbound Server PUBREL
// ---------------------------------------------------------------------------

/// When the server delivers a `QoS` 2 message to a subscriber, the PUBREL
/// it sends MUST have fixed header flags = `0x02` (byte `0x62`).
#[tokio::test]
async fn server_pubrel_correct_flags() {
    let broker = ConformanceBroker::start().await;

    let mut raw_sub = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("srv-pubrel-sub");
    raw_sub.connect_and_establish(&sub_id, TIMEOUT).await;
    raw_sub
        .send_raw(&RawPacketBuilder::subscribe("test/srv-pubrel", 2))
        .await
        .unwrap();
    let _ = raw_sub.read_packet_bytes(TIMEOUT).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("srv-pubrel-pub", &broker).await;
    let pub_opts = mqtt5::PublishOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options("test/srv-pubrel", b"outbound-qos2".to_vec(), pub_opts)
        .await
        .unwrap();

    let pub_packet_id = raw_sub
        .expect_publish_qos2(TIMEOUT)
        .await
        .expect("expected QoS 2 PUBLISH from server");

    raw_sub
        .send_raw(&RawPacketBuilder::pubrec(pub_packet_id))
        .await
        .unwrap();

    let (first_byte, rel_id, reason) = raw_sub
        .expect_pubrel_raw(TIMEOUT)
        .await
        .expect("expected PUBREL from server");

    assert_eq!(
        first_byte, 0x62,
        "[MQTT-3.6.1-1] server PUBREL first byte must be 0x62 (flags=0x02)"
    );
    assert_eq!(rel_id, pub_packet_id, "PUBREL packet ID must match PUBLISH");
    assert!(
        reason == 0x00 || reason == 0x92,
        "PUBREL reason code must be valid (got 0x{reason:02X})"
    );

    raw_sub
        .send_raw(&RawPacketBuilder::pubcomp(pub_packet_id))
        .await
        .unwrap();
}
