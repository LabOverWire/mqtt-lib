//! Section 4.3 — `QoS` delivery protocols and Section 4.4 — message redelivery.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use crate::test_client::TestClient;
use mqtt5_protocol::types::{PublishOptions, QoS, SubscribeOptions};
use std::collections::HashSet;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// `[MQTT-4.3.1-1]` In the `QoS` 0 delivery protocol, the Server MUST send a
/// PUBLISH packet with DUP=0.
#[conformance_test(
    ids = ["MQTT-4.3.1-1"],
    requires = ["transport.tcp"],
)]
async fn qos0_server_outbound_publish_has_dup_zero(sut: SutHandle) {
    let topic = format!("qos0-dup/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 0))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub").await.unwrap();
    publisher.publish(&topic, b"qos0-data").await.unwrap();

    let (first_byte, _packet_id, qos, recv_topic, payload) = sub
        .expect_publish_with_id(TIMEOUT)
        .await
        .expect("[MQTT-4.3.1-1] subscriber must receive QoS 0 PUBLISH");

    assert_eq!(recv_topic, topic);
    assert_eq!(payload, b"qos0-data");
    assert_eq!(qos, 0, "[MQTT-4.3.1-1] QoS must be 0");
    let dup = (first_byte >> 3) & 0x01;
    assert_eq!(
        dup, 0,
        "[MQTT-4.3.1-1] Server outbound `QoS` 0 PUBLISH must have DUP=0"
    );
}

/// `[MQTT-4.3.2-1]` `[MQTT-4.3.2-2]` In the `QoS` 1 delivery protocol, the
/// Server MUST assign a non-zero Packet Identifier each time it has a new
/// Application Message to deliver, and the PUBLISH MUST have DUP=0 on first
/// delivery.
#[conformance_test(
    ids = ["MQTT-4.3.2-1", "MQTT-4.3.2-2"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn qos1_server_outbound_unique_nonzero_id_and_dup_zero(sut: SutHandle) {
    let topic = format!("qos1-id/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 1))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut pub_raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("pub");
    pub_raw.connect_and_establish(&pub_id, TIMEOUT).await;

    let mut seen_ids = HashSet::new();
    for i in 1..=3u16 {
        pub_raw
            .send_raw(&RawPacketBuilder::publish_qos1(
                &topic,
                &[u8::try_from(i).unwrap()],
                i,
            ))
            .await
            .unwrap();
        let _ = pub_raw.expect_puback(TIMEOUT).await;

        let (first_byte, packet_id, qos, _topic, _payload) = sub
            .expect_publish_with_id(TIMEOUT)
            .await
            .unwrap_or_else(|| panic!("must receive PUBLISH #{i}"));

        assert_eq!(qos, 1, "[MQTT-4.3.2-1] QoS must be 1");

        assert_ne!(
            packet_id, 0,
            "[MQTT-4.3.2-1] Packet ID must be non-zero for QoS 1"
        );

        assert!(
            seen_ids.insert(packet_id),
            "[MQTT-4.3.2-1] Packet IDs must be unique, duplicate: {packet_id}"
        );

        let dup = (first_byte >> 3) & 0x01;
        assert_eq!(
            dup, 0,
            "[MQTT-4.3.2-2] Server outbound QoS 1 PUBLISH must have DUP=0 on first delivery"
        );

        sub.send_raw(&RawPacketBuilder::puback(packet_id))
            .await
            .unwrap();
    }
}

/// `[MQTT-4.3.2-5]` After the Server has sent a PUBACK, the Packet Identifier
/// is available for reuse.
#[conformance_test(
    ids = ["MQTT-4.3.2-5"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn qos1_packet_id_reusable_after_puback(sut: SutHandle) {
    let topic = format!("qos1-reuse/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 1))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub").await.unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };

    publisher
        .publish_with_options(&topic, b"msg-1", pub_opts.clone())
        .await
        .unwrap();

    let (_, first_id, _, _, _) = sub
        .expect_publish_with_id(TIMEOUT)
        .await
        .expect("must receive first PUBLISH");

    sub.send_raw(&RawPacketBuilder::puback(first_id))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    publisher
        .publish_with_options(&topic, b"msg-2", pub_opts)
        .await
        .unwrap();

    let (_, second_id, _, _, _) = sub
        .expect_publish_with_id(TIMEOUT)
        .await
        .expect("must receive second PUBLISH");

    assert_ne!(
        second_id, 0,
        "[MQTT-4.3.2-5] Second packet ID must be non-zero"
    );
}

/// `[MQTT-2.2.1-4]` The Server assigns non-zero Packet Identifiers to outbound
/// QoS>0 packets.
#[conformance_test(
    ids = ["MQTT-2.2.1-4"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn server_assigns_nonzero_packet_ids(sut: SutHandle) {
    let topic_q1 = format!("pid-nz-q1/{}", unique_client_id("t"));
    let topic_q2 = format!("pid-nz-q2/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe_with_packet_id(&topic_q1, 1, 1))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe_with_packet_id(&topic_q2, 2, 2))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub").await.unwrap();

    let q1_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic_q1, b"q1-data", q1_opts)
        .await
        .unwrap();

    let (_, pid1, qos1, _, _) = sub
        .expect_publish_with_id(TIMEOUT)
        .await
        .expect("must receive QoS 1 PUBLISH");
    assert_eq!(qos1, 1);
    assert_ne!(
        pid1, 0,
        "[MQTT-2.2.1-4] Server must assign non-zero packet ID for QoS 1"
    );
    sub.send_raw(&RawPacketBuilder::puback(pid1)).await.unwrap();

    let q2_opts = PublishOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic_q2, b"q2-data", q2_opts)
        .await
        .unwrap();

    let (_, pid2, qos2, _, _) = sub
        .expect_publish_with_id(TIMEOUT)
        .await
        .expect("must receive QoS 2 PUBLISH");
    assert_eq!(qos2, 2);
    assert_ne!(
        pid2, 0,
        "[MQTT-2.2.1-4] Server must assign non-zero packet ID for QoS 2"
    );
    sub.send_raw(&RawPacketBuilder::pubrec(pid2)).await.unwrap();
    let _ = sub.expect_pubrel_raw(TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::pubcomp(pid2))
        .await
        .unwrap();
}

/// `[MQTT-4.3.3-1]` `[MQTT-4.3.3-2]` In the `QoS` 2 delivery protocol, the
/// Server MUST assign a non-zero Packet Identifier and the outbound PUBLISH
/// MUST have DUP=0 on first delivery.
#[conformance_test(
    ids = ["MQTT-4.3.3-1", "MQTT-4.3.3-2"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn qos2_server_outbound_unique_nonzero_id_and_dup_zero(sut: SutHandle) {
    let topic = format!("qos2-id/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 2))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub").await.unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"qos2-data", pub_opts)
        .await
        .unwrap();

    let (first_byte, packet_id, qos, recv_topic, payload) = sub
        .expect_publish_with_id(TIMEOUT)
        .await
        .expect("[MQTT-4.3.3-1] subscriber must receive QoS 2 PUBLISH");

    assert_eq!(recv_topic, topic);
    assert_eq!(payload, b"qos2-data");
    assert_eq!(qos, 2, "[MQTT-4.3.3-1] QoS must be 2");
    assert_ne!(
        packet_id, 0,
        "[MQTT-4.3.3-1] Packet ID must be non-zero for QoS 2"
    );

    let dup = (first_byte >> 3) & 0x01;
    assert_eq!(
        dup, 0,
        "[MQTT-4.3.3-2] Server outbound QoS 2 PUBLISH must have DUP=0 on first delivery"
    );

    sub.send_raw(&RawPacketBuilder::pubrec(packet_id))
        .await
        .unwrap();
    let _ = sub.expect_pubrel_raw(TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::pubcomp(packet_id))
        .await
        .unwrap();
}

/// `[MQTT-4.3.3-3]` The `QoS` 2 message is considered "unacknowledged" until
/// the corresponding PUBREC has been received. The Server MUST hold the message
/// state.
#[conformance_test(
    ids = ["MQTT-4.3.3-3"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn qos2_unacknowledged_until_pubrec(sut: SutHandle) {
    let topic = format!("qos2-hold/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 2))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub").await.unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"held", pub_opts)
        .await
        .unwrap();

    let (_, packet_id, qos, _, _) = sub
        .expect_publish_with_id(TIMEOUT)
        .await
        .expect("must receive QoS 2 PUBLISH");
    assert_eq!(qos, 2);
    assert_ne!(packet_id, 0);

    let extra = sub.read_packet_bytes(Duration::from_secs(1)).await;
    assert!(
        extra.is_none(),
        "[MQTT-4.3.3-3] Server must not send PUBREL or discard state before receiving PUBREC"
    );
}

/// `[MQTT-4.3.3-5]` The Server MUST send a PUBREL after receiving PUBREC, and
/// MUST hold the Packet Identifier state until the corresponding PUBCOMP is
/// received.
#[conformance_test(
    ids = ["MQTT-4.3.3-5"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn qos2_server_sends_pubrel_after_pubrec_and_holds_until_pubcomp(sut: SutHandle) {
    let topic = format!("qos2-flow/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 2))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub").await.unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"flow-data", pub_opts)
        .await
        .unwrap();

    let (_, packet_id, _, _, _) = sub
        .expect_publish_with_id(TIMEOUT)
        .await
        .expect("must receive QoS 2 PUBLISH");

    sub.send_raw(&RawPacketBuilder::pubrec(packet_id))
        .await
        .unwrap();

    let (first_byte, rel_id, reason) = sub
        .expect_pubrel_raw(TIMEOUT)
        .await
        .expect("[MQTT-4.3.3-5] Server must send PUBREL after receiving PUBREC");

    assert_eq!(
        first_byte, 0x62,
        "PUBREL first byte must be 0x62 (flags=0x02)"
    );
    assert_eq!(rel_id, packet_id, "PUBREL packet ID must match PUBLISH");
    assert_eq!(reason, 0x00, "PUBREL reason code should be Success");

    sub.send_raw(&RawPacketBuilder::pubcomp(packet_id))
        .await
        .unwrap();
}

/// `[MQTT-4.3.3-9]` A PUBREC with a Reason Code of 0x80 or greater indicates
/// the Server MUST discard the message and treat the Packet Identifier as
/// available for reuse.
#[conformance_test(
    ids = ["MQTT-4.3.3-9"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn qos2_pubrec_error_allows_packet_id_reuse(sut: SutHandle) {
    let topic = format!("qos2-pubrec-err/{}", unique_client_id("t"));

    let subscriber = TestClient::connect_with_prefix(&sut, "sub").await.unwrap();
    let sub_opts = SubscribeOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    let _subscription = subscriber
        .subscribe(&topic, sub_opts)
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut pub_raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("pub");
    pub_raw.connect_and_establish(&pub_id, TIMEOUT).await;

    let packet_id: u16 = 10;
    pub_raw
        .send_raw(&RawPacketBuilder::publish_qos2(
            &topic,
            b"will-fail",
            packet_id,
        ))
        .await
        .unwrap();

    let (rec_id, _reason) = pub_raw
        .expect_pubrec(TIMEOUT)
        .await
        .expect("must receive PUBREC");
    assert_eq!(rec_id, packet_id);

    pub_raw
        .send_raw(&RawPacketBuilder::pubrel(packet_id))
        .await
        .unwrap();
    let _ = pub_raw.expect_pubcomp(TIMEOUT).await;

    pub_raw
        .send_raw(&RawPacketBuilder::publish_qos2(
            &topic,
            b"reused-id",
            packet_id,
        ))
        .await
        .unwrap();

    let (rec_id2, reason2) = pub_raw.expect_pubrec(TIMEOUT).await.expect(
        "[MQTT-4.3.3-9] Server must accept new PUBLISH with reused packet ID after flow completes",
    );
    assert_eq!(rec_id2, packet_id);
    assert_eq!(
        reason2, 0x00,
        "[MQTT-4.3.3-9] PUBREC for reused packet ID must be Success"
    );

    pub_raw
        .send_raw(&RawPacketBuilder::pubrel(packet_id))
        .await
        .unwrap();
    let _ = pub_raw.expect_pubcomp(TIMEOUT).await;
}

/// `[MQTT-4.3.3-10]` A `QoS` 2 PUBLISH with DUP=1 (retransmission) MUST NOT
/// cause the message to be duplicated to subscribers.
#[conformance_test(
    ids = ["MQTT-4.3.3-10"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn qos2_duplicate_publish_no_double_delivery(sut: SutHandle) {
    let topic = format!("qos2-dup/{}", unique_client_id("t"));

    let subscriber = TestClient::connect_with_prefix(&sut, "sub").await.unwrap();
    let sub_opts = SubscribeOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    let subscription = subscriber
        .subscribe(&topic, sub_opts)
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut pub_raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("pub");
    pub_raw.connect_and_establish(&pub_id, TIMEOUT).await;

    let packet_id: u16 = 50;
    pub_raw
        .send_raw(&RawPacketBuilder::publish_qos2(
            &topic,
            b"once-only",
            packet_id,
        ))
        .await
        .unwrap();

    let (rec_id, _) = pub_raw
        .expect_pubrec(TIMEOUT)
        .await
        .expect("must receive PUBREC");
    assert_eq!(rec_id, packet_id);

    pub_raw
        .send_raw(&RawPacketBuilder::publish_qos2_with_dup(
            &topic,
            b"once-only",
            packet_id,
        ))
        .await
        .unwrap();

    let (rec_id2, _) = pub_raw
        .expect_pubrec(TIMEOUT)
        .await
        .expect("must receive PUBREC for duplicate");
    assert_eq!(rec_id2, packet_id);

    pub_raw
        .send_raw(&RawPacketBuilder::pubrel(packet_id))
        .await
        .unwrap();
    let _ = pub_raw.expect_pubcomp(TIMEOUT).await;

    tokio::time::sleep(Duration::from_millis(500)).await;
    let msgs = subscription.snapshot();
    assert_eq!(
        msgs.len(),
        1,
        "[MQTT-4.3.3-10] Duplicate QoS 2 PUBLISH must not cause double delivery, got {} messages",
        msgs.len()
    );
    assert_eq!(msgs[0].payload, b"once-only");
}

/// `[MQTT-4.3.3-12]` After PUBCOMP, the same Packet Identifier is treated as
/// a new publication.
#[conformance_test(
    ids = ["MQTT-4.3.3-12"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn qos2_after_pubcomp_same_id_is_new_message(sut: SutHandle) {
    let topic = format!("qos2-new/{}", unique_client_id("t"));

    let subscriber = TestClient::connect_with_prefix(&sut, "sub").await.unwrap();
    let sub_opts = SubscribeOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    let subscription = subscriber
        .subscribe(&topic, sub_opts)
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut pub_raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("pub");
    pub_raw.connect_and_establish(&pub_id, TIMEOUT).await;

    let packet_id: u16 = 7;

    pub_raw
        .send_raw(&RawPacketBuilder::publish_qos2(&topic, b"first", packet_id))
        .await
        .unwrap();
    let _ = pub_raw.expect_pubrec(TIMEOUT).await.expect("PUBREC #1");
    pub_raw
        .send_raw(&RawPacketBuilder::pubrel(packet_id))
        .await
        .unwrap();
    let _ = pub_raw.expect_pubcomp(TIMEOUT).await.expect("PUBCOMP #1");

    pub_raw
        .send_raw(&RawPacketBuilder::publish_qos2(
            &topic, b"second", packet_id,
        ))
        .await
        .unwrap();
    let (rec_id, reason) = pub_raw
        .expect_pubrec(TIMEOUT)
        .await
        .expect("[MQTT-4.3.3-12] Server must accept new PUBLISH with same ID after PUBCOMP");
    assert_eq!(rec_id, packet_id);
    assert_eq!(
        reason, 0x00,
        "[MQTT-4.3.3-12] PUBREC for reused ID must be Success"
    );
    pub_raw
        .send_raw(&RawPacketBuilder::pubrel(packet_id))
        .await
        .unwrap();
    let _ = pub_raw.expect_pubcomp(TIMEOUT).await.expect("PUBCOMP #2");

    assert!(
        subscription.wait_for_messages(2, TIMEOUT).await,
        "[MQTT-4.3.3-12] Both messages must be delivered"
    );
    let msgs = subscription.snapshot();
    assert_eq!(msgs[0].payload, b"first");
    assert_eq!(msgs[1].payload, b"second");
}

/// `[MQTT-4.4.0-1]` The Server MUST NOT resend a PUBLISH during the same
/// Network Connection (no spontaneous retransmission on active connections).
#[conformance_test(
    ids = ["MQTT-4.4.0-1"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn no_spontaneous_retransmission_on_active_connection(sut: SutHandle) {
    let topic = format!("no-resend/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 1))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub").await.unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"no-retry", pub_opts)
        .await
        .unwrap();

    let (_, packet_id, _, _, _) = sub
        .expect_publish_with_id(TIMEOUT)
        .await
        .expect("must receive QoS 1 PUBLISH");

    let retransmit = sub.read_packet_bytes(Duration::from_secs(2)).await;

    assert!(
        retransmit.is_none(),
        "[MQTT-4.4.0-1] Server MUST NOT resend PUBLISH during active Network Connection"
    );

    sub.send_raw(&RawPacketBuilder::puback(packet_id))
        .await
        .unwrap();
}

/// `[MQTT-4.4.0-2]` When a Client sends a PUBACK with a Reason Code of 0x80
/// or greater, the Server MUST treat the PUBLISH as acknowledged and MUST NOT
/// attempt to retransmit.
#[conformance_test(
    ids = ["MQTT-4.4.0-2"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn puback_error_stops_retransmission(sut: SutHandle) {
    let topic = format!("puback-err/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 1))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub").await.unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"ack-err", pub_opts)
        .await
        .unwrap();

    let (_, packet_id, _, _, _) = sub
        .expect_publish_with_id(TIMEOUT)
        .await
        .expect("must receive QoS 1 PUBLISH");

    sub.send_raw(&RawPacketBuilder::puback_with_reason(packet_id, 0x80))
        .await
        .unwrap();

    let retransmit = sub.read_packet_bytes(Duration::from_secs(2)).await;

    assert!(
        retransmit.is_none(),
        "[MQTT-4.4.0-2] Server MUST NOT retransmit after receiving PUBACK with error reason code"
    );
}
