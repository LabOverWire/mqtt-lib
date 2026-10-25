//! Sections 3.10–3.11 — UNSUBSCRIBE and UNSUBACK.

use crate::assertions;
use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use crate::test_client::TestClient;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// `[MQTT-3.10.1-1]` UNSUBSCRIBE fixed header flags MUST be `0x02`.
/// A raw UNSUBSCRIBE with flags `0x00` (byte `0xA0`) must cause disconnect.
#[conformance_test(
    ids = ["MQTT-3.10.1-1"],
    requires = ["transport.tcp"],
)]
async fn unsubscribe_invalid_flags_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("unsub-flags");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::unsubscribe_invalid_flags(
        "test/topic",
        1,
    ))
    .await
    .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.10.1-1] server must disconnect on UNSUBSCRIBE with invalid flags"
    );
}

/// `[MQTT-3.10.3-2]` `[MQTT-4.13.1-1]` UNSUBSCRIBE payload MUST contain at
/// least one topic filter.
#[conformance_test(
    ids = ["MQTT-3.10.3-2", "MQTT-4.13.1-1"],
    requires = ["transport.tcp"],
)]
async fn unsubscribe_empty_payload_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("unsub-empty");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::unsubscribe_empty_payload(1))
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.10.3-2] server must disconnect on UNSUBSCRIBE with no topic filters"
    );
}

/// `[MQTT-2.2.1-6]` `[MQTT-3.10.4-4]` `[MQTT-3.10.4-5]` The Server MUST answer
/// an UNSUBSCRIBE with an UNSUBACK carrying the same Packet Identifier.
#[conformance_test(
    ids = ["MQTT-2.2.1-6", "MQTT-3.10.4-4", "MQTT-3.10.4-5"],
    requires = ["transport.tcp"],
)]
async fn unsuback_packet_id_matches(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("unsuback-pid");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/unsuback-pid",
        0,
        1,
    ))
    .await
    .unwrap();
    raw.expect_suback(TIMEOUT).await.expect("expected SUBACK");

    let packet_id: u16 = 42;
    raw.send_raw(&RawPacketBuilder::unsubscribe(
        "test/unsuback-pid",
        packet_id,
    ))
    .await
    .unwrap();

    let (ack_id, reason_codes) = raw
        .expect_unsuback(TIMEOUT)
        .await
        .expect("expected UNSUBACK from broker");

    assert_eq!(
        ack_id, packet_id,
        "[MQTT-3.10.4-5] UNSUBACK packet ID must match UNSUBSCRIBE packet ID"
    );
    assert_eq!(
        reason_codes.len(),
        1,
        "UNSUBACK must contain one reason code"
    );
}

/// `[MQTT-3.10.4-6]` `[MQTT-3.11.3-2]` An UNSUBSCRIBE with several Topic
/// Filters must be answered with one UNSUBACK holding an UNSUBSCRIBE Reason
/// Code for each filter.
#[conformance_test(
    ids = ["MQTT-3.10.4-6", "MQTT-3.11.3-2"],
    requires = ["transport.tcp"],
)]
async fn unsuback_reason_codes_per_filter(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("unsuback-multi");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    let filters = ["test/a", "test/b", "test/c"];
    raw.send_raw(&RawPacketBuilder::unsubscribe_multiple(&filters, 10))
        .await
        .unwrap();

    let (ack_id, reason_codes) = raw
        .expect_unsuback(TIMEOUT)
        .await
        .expect("expected UNSUBACK from broker");

    assert_eq!(ack_id, 10);
    assert_eq!(
        reason_codes.len(),
        3,
        "[MQTT-3.11.3-2] UNSUBACK must contain one reason code per topic filter"
    );
}

/// `[MQTT-3.11.3-2]` Subscribe then unsubscribe — reason code should be
/// Success (0x00).
#[conformance_test(
    ids = ["MQTT-3.11.3-2"],
    requires = ["transport.tcp"],
)]
async fn unsuback_success_for_existing(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("unsuback-ok");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/unsuback-ok",
        0,
        1,
    ))
    .await
    .unwrap();
    raw.expect_suback(TIMEOUT).await.expect("expected SUBACK");

    raw.send_raw(&RawPacketBuilder::unsubscribe("test/unsuback-ok", 2))
        .await
        .unwrap();

    let (_, reason_codes) = raw
        .expect_unsuback(TIMEOUT)
        .await
        .expect("expected UNSUBACK from broker");

    assert_eq!(
        reason_codes[0], 0x00,
        "UNSUBACK for existing subscription should be Success (0x00), got 0x{:02X}",
        reason_codes[0]
    );
}

/// `[MQTT-3.10.4-5]` `[MQTT-3.11.3-2]` An UNSUBSCRIBE for a filter that was
/// never subscribed must still get an UNSUBACK, with Reason Code 0x11 (No
/// subscription existed).
#[conformance_test(
    ids = ["MQTT-3.10.4-5", "MQTT-3.11.3-2"],
    requires = ["transport.tcp"],
)]
async fn unsuback_no_subscription_existed(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("unsuback-noexist");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::unsubscribe("test/never-subscribed", 1))
        .await
        .unwrap();

    let (_, reason_codes) = raw
        .expect_unsuback(TIMEOUT)
        .await
        .expect("expected UNSUBACK from broker");

    assert_eq!(
        reason_codes[0], 0x11,
        "UNSUBACK for non-existent subscription should be NoSubscriptionExisted (0x11), got 0x{:02X}",
        reason_codes[0]
    );
}

/// `[MQTT-3.10.4-1]` `[MQTT-3.10.4-2]` After an UNSUBSCRIBE that matches a
/// subscription exactly, the Server MUST delete it and stop sending messages
/// for it.
#[conformance_test(
    ids = ["MQTT-3.10.4-1", "MQTT-3.10.4-2"],
    requires = ["transport.tcp"],
)]
async fn unsubscribe_stops_delivery(sut: SutHandle) {
    let topic = format!("unsub-stop/{}", unique_client_id("t"));
    let mut subscriber =
        assertions::subscribed_raw_client(&sut, "unsub-stop", &topic, TIMEOUT).await;
    let mut witness =
        assertions::subscribed_raw_client(&sut, "unsub-stop-witness", &topic, TIMEOUT).await;

    let publisher = TestClient::connect_with_prefix(&sut, "unsub-stop-pub")
        .await
        .unwrap();
    publisher.publish(&topic, b"before").await.unwrap();
    assert!(
        assertions::first_publish_matching(&mut subscriber, TIMEOUT, |_| true)
            .await
            .is_some(),
        "the subscriber must receive the message before unsubscribing"
    );
    assert!(
        assertions::first_publish_matching(&mut witness, TIMEOUT, |_| true)
            .await
            .is_some(),
        "the witness must receive the message before the unsubscribe"
    );

    subscriber
        .send_raw(&RawPacketBuilder::unsubscribe(&topic, 2))
        .await
        .unwrap();
    let (_, reason_codes) = subscriber
        .expect_unsuback(TIMEOUT)
        .await
        .expect("expected UNSUBACK");
    assert_eq!(
        reason_codes,
        [0x00],
        "[MQTT-3.10.4-1] the exactly matching subscription must be deleted"
    );

    publisher.publish(&topic, b"after").await.unwrap();
    assert!(
        assertions::first_publish_matching(&mut witness, TIMEOUT, |_| true)
            .await
            .is_some(),
        "the witness must receive the message published after the unsubscribe"
    );
    assertions::expect_no_publish(
        &mut subscriber,
        "[MQTT-3.10.4-2]",
        Duration::from_millis(300),
        |_| true,
    )
    .await;
}

/// `[MQTT-3.10.4-1]` `[MQTT-3.10.4-2]` `[MQTT-3.10.4-6]` `[MQTT-3.11.3-1]`
/// `[MQTT-3.11.3-2]` An UNSUBSCRIBE with one existing and one non-existing
/// filter must get one UNSUBACK with 0x00 and 0x11 in filter order, and
/// messages must stop for the removed filter only.
#[conformance_test(
    ids = ["MQTT-3.10.4-1", "MQTT-3.10.4-2", "MQTT-3.10.4-6", "MQTT-3.11.3-1", "MQTT-3.11.3-2"],
    requires = ["transport.tcp"],
)]
async fn unsubscribe_partial_multi(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("unsub-partial");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_multiple(
        &[("test/keep", 0), ("test/remove", 0)],
        1,
    ))
    .await
    .unwrap();
    raw.expect_suback(TIMEOUT).await.expect("expected SUBACK");

    let filters = ["test/remove", "test/never-existed"];
    raw.send_raw(&RawPacketBuilder::unsubscribe_multiple(&filters, 2))
        .await
        .unwrap();

    let (_, reason_codes) = raw
        .expect_unsuback(TIMEOUT)
        .await
        .expect("expected UNSUBACK from broker");

    assert_eq!(reason_codes.len(), 2, "one reason code per filter");
    assert_eq!(
        reason_codes[0], 0x00,
        "first filter (existed) should be Success (0x00), got 0x{:02X}",
        reason_codes[0]
    );
    assert_eq!(
        reason_codes[1], 0x11,
        "second filter (never existed) should be NoSubscriptionExisted (0x11), got 0x{:02X}",
        reason_codes[1]
    );

    let publisher = TestClient::connect_with_prefix(&sut, "unsub-partial-pub")
        .await
        .unwrap();
    publisher.publish("test/keep", b"still-here").await.unwrap();

    let msg = raw.expect_publish(TIMEOUT).await;
    assert!(
        msg.is_some(),
        "messages on test/keep should still be delivered"
    );

    publisher.publish("test/remove", b"gone").await.unwrap();

    let stale = raw.expect_publish(Duration::from_millis(500)).await;
    assert!(
        stale.is_none(),
        "messages on test/remove should not be delivered after unsubscribe"
    );
}

/// `[MQTT-3.10.4-1]` `[MQTT-3.11.3-2]` Unsubscribing twice from the same filter
/// must get 0x00 the first time and 0x11 (No subscription existed) the second.
#[conformance_test(
    ids = ["MQTT-3.10.4-1", "MQTT-3.11.3-2"],
    requires = ["transport.tcp"],
)]
async fn unsubscribe_idempotent(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("unsub-idempotent");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/idempotent",
        0,
        1,
    ))
    .await
    .unwrap();
    raw.expect_suback(TIMEOUT).await.expect("expected SUBACK");

    raw.send_raw(&RawPacketBuilder::unsubscribe("test/idempotent", 2))
        .await
        .unwrap();
    let (_, rc1) = raw
        .expect_unsuback(TIMEOUT)
        .await
        .expect("expected first UNSUBACK");
    assert_eq!(
        rc1[0], 0x00,
        "first unsubscribe should be Success (0x00), got 0x{:02X}",
        rc1[0]
    );

    raw.send_raw(&RawPacketBuilder::unsubscribe("test/idempotent", 3))
        .await
        .unwrap();
    let (_, rc2) = raw
        .expect_unsuback(TIMEOUT)
        .await
        .expect("expected second UNSUBACK");
    assert_eq!(
        rc2[0], 0x11,
        "second unsubscribe should be NoSubscriptionExisted (0x11), got 0x{:02X}",
        rc2[0]
    );
}

/// `[MQTT-3.11.3-1]` `[MQTT-3.11.3-2]` The UNSUBACK Reason Codes for a
/// well-formed UNSUBSCRIBE must be UNSUBSCRIBE Reason Code values, in the order
/// of the Topic Filters.
#[conformance_test(
    ids = ["MQTT-3.11.3-1", "MQTT-3.11.3-2"],
    requires = ["transport.tcp"],
)]
async fn unsuback_reason_codes_are_valid_spec_values(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("unsuback-valid");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/valid-rc",
        0,
        1,
    ))
    .await
    .unwrap();
    raw.expect_suback(TIMEOUT).await.expect("SUBACK");

    let filters = ["test/valid-rc", "test/never-existed-rc"];
    raw.send_raw(&RawPacketBuilder::unsubscribe_multiple(&filters, 2))
        .await
        .unwrap();

    let (_, reason_codes) = raw
        .expect_unsuback(TIMEOUT)
        .await
        .expect("expected UNSUBACK from broker");

    assert_eq!(reason_codes.len(), 2);

    let valid_unsuback_codes = [0x00, 0x11, 0x80, 0x83, 0x87, 0x8F, 0x91];
    for (i, rc) in reason_codes.iter().enumerate() {
        assert!(
            valid_unsuback_codes.contains(rc),
            "[MQTT-3.11.3-2] UNSUBACK reason code {i} is 0x{rc:02X}, not a valid spec value"
        );
    }
    assert_eq!(
        reason_codes[0], 0x00,
        "[MQTT-3.11.3-1] first filter (subscribed) should be Success (0x00)"
    );
    assert_eq!(
        reason_codes[1], 0x11,
        "[MQTT-3.11.3-1] second filter (never existed) should be NoSubscriptionExisted (0x11)"
    );
}

/// `[MQTT-3.10.3-1]` `[MQTT-4.13.1-1]` Topic filter in UNSUBSCRIBE must be
/// valid UTF-8. Sending invalid UTF-8 bytes must cause disconnect.
#[conformance_test(
    ids = ["MQTT-3.10.3-1", "MQTT-4.13.1-1"],
    requires = ["transport.tcp"],
)]
async fn unsubscribe_invalid_utf8_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("unsub-bad-utf8");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::unsubscribe_invalid_utf8(1))
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.10.3-1] server must disconnect on UNSUBSCRIBE with invalid UTF-8 topic filter"
    );
}
