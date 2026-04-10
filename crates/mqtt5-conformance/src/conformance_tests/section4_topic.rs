//! Section 4.7 — Topic Names and Topic Filters.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use crate::test_client::TestClient;
use mqtt5_protocol::types::SubscribeOptions;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// `[MQTT-4.7.1-1]` The multi-level wildcard `#` MUST be the last character
/// in a topic filter.
#[conformance_test(
    ids = ["MQTT-4.7.1-1"],
    requires = ["transport.tcp"],
)]
async fn multi_level_wildcard_must_be_last(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("mlwild-last");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "sport/tennis/#/ranking",
        0,
        1,
    ))
    .await
    .unwrap();

    let response = raw.expect_suback(TIMEOUT).await;
    match response {
        Some((_, reason_codes)) => {
            assert_eq!(reason_codes.len(), 1);
            assert_eq!(
                reason_codes[0], 0x8F,
                "[MQTT-4.7.1-1] # not last must return TopicFilterInvalid (0x8F)"
            );
        }
        None => {
            assert!(
                raw.expect_disconnect(Duration::from_millis(100)).await,
                "[MQTT-4.7.1-1] # not last must cause SUBACK 0x8F or disconnect"
            );
        }
    }
}

/// `[MQTT-4.7.1-1]` The multi-level wildcard `#` MUST occupy a complete
/// topic level on its own.
#[conformance_test(
    ids = ["MQTT-4.7.1-1"],
    requires = ["transport.tcp"],
)]
async fn multi_level_wildcard_must_be_full_level(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("mlwild-full");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "sport/tennis#",
        0,
        1,
    ))
    .await
    .unwrap();

    let response = raw.expect_suback(TIMEOUT).await;
    match response {
        Some((_, reason_codes)) => {
            assert_eq!(reason_codes.len(), 1);
            assert_eq!(
                reason_codes[0], 0x8F,
                "[MQTT-4.7.1-1] tennis# (not full level) must return TopicFilterInvalid"
            );
        }
        None => {
            assert!(
                raw.expect_disconnect(Duration::from_millis(100)).await,
                "[MQTT-4.7.1-1] tennis# must cause SUBACK 0x8F or disconnect"
            );
        }
    }
}

/// `[MQTT-4.7.1-2]` The single-level wildcard `+` MUST occupy a complete
/// topic level on its own.
#[conformance_test(
    ids = ["MQTT-4.7.1-2"],
    requires = ["transport.tcp"],
)]
async fn single_level_wildcard_must_be_full_level(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("slwild-full");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_multiple(
        &[("sport+", 0), ("sport/+tennis", 0)],
        1,
    ))
    .await
    .unwrap();

    let response = raw.expect_suback(TIMEOUT).await;
    match response {
        Some((_, reason_codes)) => {
            assert_eq!(reason_codes.len(), 2);
            assert_eq!(
                reason_codes[0], 0x8F,
                "[MQTT-4.7.1-2] sport+ must return TopicFilterInvalid"
            );
            assert_eq!(
                reason_codes[1], 0x8F,
                "[MQTT-4.7.1-2] sport/+tennis must return TopicFilterInvalid"
            );
        }
        None => {
            assert!(
                raw.expect_disconnect(Duration::from_millis(100)).await,
                "[MQTT-4.7.1-2] invalid + usage must cause SUBACK 0x8F or disconnect"
            );
        }
    }
}

/// `[MQTT-4.7.1-1]` `[MQTT-4.7.1-2]` Valid wildcard filters must be
/// accepted with reason code Success (0x00).
#[conformance_test(
    ids = ["MQTT-4.7.1-1", "MQTT-4.7.1-2"],
    requires = ["transport.tcp"],
)]
async fn valid_wildcard_filters_accepted(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("wild-ok");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_multiple(
        &[
            ("sport/+", 0),
            ("sport/#", 0),
            ("+/tennis/#", 0),
            ("#", 0),
            ("+", 0),
        ],
        1,
    ))
    .await
    .unwrap();

    let (_, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("must receive SUBACK for valid wildcards");
    assert_eq!(reason_codes.len(), 5);
    for (i, rc) in reason_codes.iter().enumerate() {
        assert_eq!(*rc, 0x00, "filter {i} must be granted QoS 0, got {rc:#04x}");
    }
}

/// `[MQTT-4.7.2-1]` Topic Names beginning with `$` MUST NOT be matched by
/// a Topic Filter starting with a wildcard character (`#` or `+`).
#[conformance_test(
    ids = ["MQTT-4.7.2-1"],
    requires = ["transport.tcp"],
)]
async fn dollar_topics_not_matched_by_root_wildcards(sut: SutHandle) {
    let sub_hash = TestClient::connect_with_prefix(&sut, "dollar-hash")
        .await
        .unwrap();
    let subscription_hash = sub_hash
        .subscribe("#", SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    let sub_plus = TestClient::connect_with_prefix(&sut, "dollar-plus")
        .await
        .unwrap();
    let subscription_plus = sub_plus
        .subscribe("+/info", SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    tokio::time::sleep(Duration::from_millis(200)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "dollar-pub")
        .await
        .unwrap();
    publisher
        .publish("$SYS/test", b"sys-payload")
        .await
        .unwrap();
    publisher.publish("$SYS/info", b"sys-info").await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let dollar_on_hash: Vec<_> = subscription_hash
        .snapshot()
        .into_iter()
        .filter(|m| m.topic.starts_with('$'))
        .collect();
    assert!(
        dollar_on_hash.is_empty(),
        "[MQTT-4.7.2-1] # must not match $-prefixed topics, got: {:?}",
        dollar_on_hash.iter().map(|m| &m.topic).collect::<Vec<_>>()
    );

    let dollar_on_plus: Vec<_> = subscription_plus
        .snapshot()
        .into_iter()
        .filter(|m| m.topic.starts_with('$'))
        .collect();
    assert!(
        dollar_on_plus.is_empty(),
        "[MQTT-4.7.2-1] +/info must not match $-prefixed topics, got: {:?}",
        dollar_on_plus.iter().map(|m| &m.topic).collect::<Vec<_>>()
    );
}

/// `[MQTT-4.7.2-1]` Topic Filters that explicitly include `$` (e.g.
/// `$SYS/#`) MAY match topics beginning with `$`.
#[conformance_test(
    ids = ["MQTT-4.7.2-1"],
    requires = ["transport.tcp"],
)]
async fn dollar_topics_matched_by_explicit_prefix(sut: SutHandle) {
    let subscriber = TestClient::connect_with_prefix(&sut, "dollar-explicit")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe("$SYS/#", SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "dollar-explpub")
        .await
        .unwrap();
    publisher.publish("$SYS/test", b"payload").await.unwrap();

    subscription.wait_for_messages(1, TIMEOUT).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let msgs = subscription.snapshot();
    let found = msgs
        .iter()
        .find(|m| m.topic == "$SYS/test")
        .expect("$SYS/# must match $SYS/test");
    assert_eq!(found.payload, b"payload");
}

/// `[MQTT-4.7.3-1]` Topic Filters MUST be at least one character long.
/// An empty filter must be rejected with `TopicFilterInvalid` or disconnect.
#[conformance_test(
    ids = ["MQTT-4.7.3-1"],
    requires = ["transport.tcp"],
)]
async fn topic_filter_must_not_be_empty(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("empty-filter");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id("", 0, 1))
        .await
        .unwrap();

    let response = raw.expect_suback(TIMEOUT).await;
    match response {
        Some((_, reason_codes)) => {
            assert_eq!(reason_codes.len(), 1);
            assert_eq!(
                reason_codes[0], 0x8F,
                "[MQTT-4.7.3-1] empty filter must return TopicFilterInvalid"
            );
        }
        None => {
            assert!(
                raw.expect_disconnect(Duration::from_millis(100)).await,
                "[MQTT-4.7.3-1] empty filter must cause SUBACK 0x8F or disconnect"
            );
        }
    }
}

/// `[MQTT-4.7.3-2]` Topic Names and Topic Filters MUST NOT include the
/// null character (U+0000). PUBLISH with a null in the topic name must
/// cause disconnect.
#[conformance_test(
    ids = ["MQTT-4.7.3-2"],
    requires = ["transport.tcp"],
)]
async fn null_char_in_topic_name_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("null-topic");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_qos0("test\0/topic", b"payload"))
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-4.7.3-2] null char in topic name must cause disconnect"
    );
}

/// `[MQTT-4.7.1-2]` The single-level wildcard `+` matches exactly one
/// topic level — not zero, not multiple.
#[conformance_test(
    ids = ["MQTT-4.7.1-2"],
    requires = ["transport.tcp"],
)]
async fn single_level_wildcard_matches_one_level(sut: SutHandle) {
    let subscriber = TestClient::connect_with_prefix(&sut, "sl-match")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe("sport/+/player", SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "sl-pub")
        .await
        .unwrap();
    publisher
        .publish("sport/tennis/player", b"match")
        .await
        .unwrap();
    publisher
        .publish("sport/tennis/doubles/player", b"no-match")
        .await
        .unwrap();

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "sport/+/player must match sport/tennis/player"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;

    let msgs = subscription.snapshot();
    assert_eq!(
        msgs.len(),
        1,
        "sport/+/player must not match sport/tennis/doubles/player"
    );
    assert_eq!(msgs[0].topic, "sport/tennis/player");
}

/// `[MQTT-4.7.1-1]` The multi-level wildcard `#` matches the parent topic
/// and any descendants.
#[conformance_test(
    ids = ["MQTT-4.7.1-1"],
    requires = ["transport.tcp"],
)]
async fn multi_level_wildcard_matches_all_descendants(sut: SutHandle) {
    let subscriber = TestClient::connect_with_prefix(&sut, "ml-match")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe("sport/#", SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "ml-pub")
        .await
        .unwrap();
    publisher.publish("sport", b"zero").await.unwrap();
    publisher.publish("sport/tennis", b"one").await.unwrap();
    publisher
        .publish("sport/tennis/player", b"two")
        .await
        .unwrap();

    assert!(
        subscription.wait_for_messages(3, TIMEOUT).await,
        "sport/# must match sport, sport/tennis, and sport/tennis/player"
    );

    let msgs = subscription.snapshot();
    let topics: Vec<&str> = msgs.iter().map(|m| m.topic.as_str()).collect();
    assert!(topics.contains(&"sport"));
    assert!(topics.contains(&"sport/tennis"));
    assert!(topics.contains(&"sport/tennis/player"));
}

/// `[MQTT-4.5.0-1]` The server MUST deliver published messages to clients
/// that have matching subscriptions, and MUST NOT deliver to non-matching
/// subscribers.
#[conformance_test(
    ids = ["MQTT-4.5.0-1"],
    requires = ["transport.tcp"],
)]
async fn server_delivers_to_matching_subscribers(sut: SutHandle) {
    let tag = unique_client_id("deliver");
    let topic = format!("deliver/{tag}");

    let sub_exact = TestClient::connect_with_prefix(&sut, "sub-exact")
        .await
        .unwrap();
    let subscription_exact = sub_exact
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    let filter_wild = format!("deliver/{tag}/+");
    let sub_wild = TestClient::connect_with_prefix(&sut, "sub-wild")
        .await
        .unwrap();
    let subscription_wild = sub_wild
        .subscribe(&filter_wild, SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    let sub_non = TestClient::connect_with_prefix(&sut, "sub-non")
        .await
        .unwrap();
    let subscription_non = sub_non
        .subscribe("other/topic", SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub-deliver")
        .await
        .unwrap();
    publisher.publish(&topic, b"match-payload").await.unwrap();

    assert!(
        subscription_exact.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-4.5.0-1] exact-match subscriber must receive the message"
    );
    let msgs = subscription_exact.snapshot();
    assert_eq!(msgs[0].payload, b"match-payload");

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        subscription_wild.count(),
        0,
        "wildcard subscriber for deliver/tag/+ must not match deliver/tag"
    );
    assert_eq!(
        subscription_non.count(),
        0,
        "non-matching subscriber must not receive the message"
    );
}

/// `[MQTT-4.6.0-5]` Message ordering MUST be preserved per topic for the
/// same `QoS` level.
#[conformance_test(
    ids = ["MQTT-4.6.0-5"],
    requires = ["transport.tcp"],
)]
async fn message_ordering_preserved_same_qos(sut: SutHandle) {
    let tag = unique_client_id("order");
    let topic = format!("order/{tag}");

    let subscriber = TestClient::connect_with_prefix(&sut, "sub-order")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub-order")
        .await
        .unwrap();
    for i in 0u32..5 {
        let payload = format!("msg-{i}");
        publisher.publish(&topic, payload.as_bytes()).await.unwrap();
    }

    assert!(
        subscription.wait_for_messages(5, TIMEOUT).await,
        "subscriber should receive all 5 messages"
    );

    let msgs = subscription.snapshot();
    for (i, msg) in msgs.iter().enumerate() {
        let expected = format!("msg-{i}");
        assert_eq!(
            msg.payload,
            expected.as_bytes(),
            "[MQTT-4.6.0-5] message {i} must be in order, expected {expected}, got {}",
            String::from_utf8_lossy(&msg.payload)
        );
    }
}

/// `[MQTT-4.7.3-4]` Topic matching MUST NOT apply Unicode normalization.
/// U+00C5 (A-ring precomposed) and U+0041 U+030A (A + combining ring)
/// must be treated as different topics.
#[conformance_test(
    ids = ["MQTT-4.7.3-4"],
    requires = ["transport.tcp"],
)]
async fn topic_matching_no_unicode_normalization(sut: SutHandle) {
    let tag = unique_client_id("unicode");

    let precomposed = format!("uni/{tag}/\u{00C5}");
    let decomposed = format!("uni/{tag}/A\u{030A}");

    let sub_pre = TestClient::connect_with_prefix(&sut, "sub-pre")
        .await
        .unwrap();
    let subscription_pre = sub_pre
        .subscribe(&precomposed, SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    let sub_dec = TestClient::connect_with_prefix(&sut, "sub-dec")
        .await
        .unwrap();
    let subscription_dec = sub_dec
        .subscribe(&decomposed, SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub-unicode")
        .await
        .unwrap();
    publisher
        .publish(&precomposed, b"precomposed")
        .await
        .unwrap();

    assert!(
        subscription_pre.wait_for_messages(1, TIMEOUT).await,
        "precomposed subscriber must receive message on precomposed topic"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        subscription_dec.count(),
        0,
        "[MQTT-4.7.3-4] decomposed subscriber must NOT receive message on precomposed topic \
         (no Unicode normalization)"
    );

    publisher.publish(&decomposed, b"decomposed").await.unwrap();

    assert!(
        subscription_dec.wait_for_messages(1, TIMEOUT).await,
        "decomposed subscriber must receive message on decomposed topic"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    let pre_msgs = subscription_pre.snapshot();
    assert_eq!(
        pre_msgs.len(),
        1,
        "[MQTT-4.7.3-4] precomposed subscriber must NOT receive message on decomposed topic"
    );
}
