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
// Group 1: UNSUBSCRIBE Structure — Section 3.10
// ---------------------------------------------------------------------------

/// `[MQTT-3.10.1-1]` UNSUBSCRIBE fixed header flags MUST be `0x02`.
/// A raw UNSUBSCRIBE with flags `0x00` (byte `0xA0`) must cause disconnect.
#[tokio::test]
async fn unsubscribe_invalid_flags_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

/// `[MQTT-3.10.3-2]` UNSUBSCRIBE payload MUST contain at least one topic filter.
/// An empty-payload UNSUBSCRIBE must cause disconnect.
#[tokio::test]
async fn unsubscribe_empty_payload_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

// ---------------------------------------------------------------------------
// Group 2: UNSUBACK Response — Section 3.11
// ---------------------------------------------------------------------------

/// `[MQTT-3.11.2-1]` UNSUBACK packet ID must match UNSUBSCRIBE packet ID.
#[tokio::test]
async fn unsuback_packet_id_matches() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
        "[MQTT-3.11.2-1] UNSUBACK packet ID must match UNSUBSCRIBE packet ID"
    );
    assert_eq!(
        reason_codes.len(),
        1,
        "UNSUBACK must contain one reason code"
    );
}

/// `[MQTT-3.11.3-1]` UNSUBACK must contain one reason code per topic filter.
#[tokio::test]
async fn unsuback_reason_codes_per_filter() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
        "[MQTT-3.11.3-1] UNSUBACK must contain one reason code per topic filter"
    );
}

/// Subscribe then unsubscribe — reason code should be Success (0x00).
#[tokio::test]
async fn unsuback_success_for_existing() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

/// Unsubscribe from a topic never subscribed — reason code should be
/// `NoSubscriptionExisted` (0x11).
#[tokio::test]
async fn unsuback_no_subscription_existed() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

// ---------------------------------------------------------------------------
// Group 3: Subscription Removal Verification
// ---------------------------------------------------------------------------

/// `[MQTT-3.10.4-1]` After unsubscribing, the broker must stop sending
/// messages for that topic filter.
#[tokio::test]
async fn unsubscribe_stops_delivery() {
    let broker = ConformanceBroker::start().await;
    let subscriber = connected_client("unsub-stop", &broker).await;
    let collector = MessageCollector::new();
    let opts = SubscribeOptions {
        qos: QoS::AtMostOnce,
        ..Default::default()
    };
    subscriber
        .subscribe_with_options("test/unsub-stop", opts, collector.callback())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("unsub-stop-pub", &broker).await;
    publisher
        .publish("test/unsub-stop", b"before".to_vec())
        .await
        .unwrap();

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "subscriber should receive message before unsubscribe"
    );

    subscriber.unsubscribe("test/unsub-stop").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    publisher
        .publish("test/unsub-stop", b"after".to_vec())
        .await
        .unwrap();

    let got_more = collector
        .wait_for_messages(2, Duration::from_millis(500))
        .await;
    assert!(
        !got_more,
        "[MQTT-3.10.4-1] subscriber must not receive messages after unsubscribe"
    );
}

/// Multi-filter UNSUBSCRIBE: one existing, one non-existing. Verify
/// reason codes (Success + `NoSubscriptionExisted`) and that messages
/// stop for unsubscribed topic but continue for remaining.
#[tokio::test]
async fn unsubscribe_partial_multi() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
    tokio::time::sleep(Duration::from_millis(100)).await;

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

    let publisher = connected_client("unsub-partial-pub", &broker).await;
    publisher
        .publish("test/keep", b"still-here".to_vec())
        .await
        .unwrap();

    let msg = raw.expect_publish(TIMEOUT).await;
    assert!(
        msg.is_some(),
        "messages on test/keep should still be delivered"
    );

    publisher
        .publish("test/remove", b"gone".to_vec())
        .await
        .unwrap();

    let stale = raw.expect_publish(Duration::from_millis(500)).await;
    assert!(
        stale.is_none(),
        "messages on test/remove should not be delivered after unsubscribe"
    );
}

/// Unsubscribe twice from the same topic. First UNSUBACK=Success,
/// second UNSUBACK=`NoSubscriptionExisted`.
#[tokio::test]
async fn unsubscribe_idempotent() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
