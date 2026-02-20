use mqtt5_conformance::harness::{
    connected_client, unique_client_id, ConformanceBroker, MessageCollector,
};
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Group 1: Topic Filter Wildcard Rules — Section 4.7.1
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multi_level_wildcard_must_be_last() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

    let (_, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("[MQTT-4.7.1-1] must receive SUBACK");
    assert_eq!(reason_codes.len(), 1);
    assert_eq!(
        reason_codes[0], 0x8F,
        "[MQTT-4.7.1-1] # not last must return TopicFilterInvalid (0x8F)"
    );
}

#[tokio::test]
async fn multi_level_wildcard_must_be_full_level() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

    let (_, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("[MQTT-4.7.1-1] must receive SUBACK");
    assert_eq!(reason_codes.len(), 1);
    assert_eq!(
        reason_codes[0], 0x8F,
        "[MQTT-4.7.1-1] tennis# (not full level) must return TopicFilterInvalid"
    );
}

#[tokio::test]
async fn single_level_wildcard_must_be_full_level() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

    let (_, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("[MQTT-4.7.1-2] must receive SUBACK");
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

#[tokio::test]
async fn valid_wildcard_filters_accepted() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

// ---------------------------------------------------------------------------
// Group 2: Dollar-Prefix Topic Matching — Section 4.7.2
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dollar_topics_not_matched_by_root_wildcards() {
    let broker = ConformanceBroker::start().await;

    let sub_hash = connected_client("dollar-hash", &broker).await;
    let collector_hash = MessageCollector::new();
    sub_hash
        .subscribe("#", collector_hash.callback())
        .await
        .unwrap();

    let sub_plus = connected_client("dollar-plus", &broker).await;
    let collector_plus = MessageCollector::new();
    sub_plus
        .subscribe("+/info", collector_plus.callback())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("dollar-pub", &broker).await;
    publisher
        .publish("$SYS/test", b"sys-payload")
        .await
        .unwrap();
    publisher.publish("$SYS/info", b"sys-info").await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        collector_hash.count().await,
        0,
        "[MQTT-4.7.2-1] # must not match $SYS/test"
    );
    assert_eq!(
        collector_plus.count().await,
        0,
        "[MQTT-4.7.2-1] +/info must not match $SYS/info"
    );
}

#[tokio::test]
async fn dollar_topics_matched_by_explicit_prefix() {
    let broker = ConformanceBroker::start().await;

    let subscriber = connected_client("dollar-explicit", &broker).await;
    let collector = MessageCollector::new();
    subscriber
        .subscribe("$SYS/#", collector.callback())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("dollar-explpub", &broker).await;
    publisher.publish("$SYS/test", b"payload").await.unwrap();

    collector.wait_for_messages(1, TIMEOUT).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let msgs = collector.get_messages().await;
    let found = msgs
        .iter()
        .find(|m| m.topic == "$SYS/test")
        .expect("$SYS/# must match $SYS/test");
    assert_eq!(found.payload, b"payload");
}

// ---------------------------------------------------------------------------
// Group 3: Topic Name/Filter Minimum Rules — Section 4.7.3
// ---------------------------------------------------------------------------

#[tokio::test]
async fn topic_filter_must_not_be_empty() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

#[tokio::test]
async fn null_char_in_topic_name_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

// ---------------------------------------------------------------------------
// Group 4: Topic Matching Correctness — Section 4.7
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_level_wildcard_matches_one_level() {
    let broker = ConformanceBroker::start().await;

    let subscriber = connected_client("sl-match", &broker).await;
    let collector = MessageCollector::new();
    subscriber
        .subscribe("sport/+/player", collector.callback())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("sl-pub", &broker).await;
    publisher
        .publish("sport/tennis/player", b"match")
        .await
        .unwrap();
    publisher
        .publish("sport/tennis/doubles/player", b"no-match")
        .await
        .unwrap();

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "sport/+/player must match sport/tennis/player"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;

    let msgs = collector.get_messages().await;
    assert_eq!(
        msgs.len(),
        1,
        "sport/+/player must not match sport/tennis/doubles/player"
    );
    assert_eq!(msgs[0].topic, "sport/tennis/player");
}

#[tokio::test]
async fn multi_level_wildcard_matches_all_descendants() {
    let broker = ConformanceBroker::start().await;

    let subscriber = connected_client("ml-match", &broker).await;
    let collector = MessageCollector::new();
    subscriber
        .subscribe("sport/#", collector.callback())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("ml-pub", &broker).await;
    publisher.publish("sport", b"zero").await.unwrap();
    publisher.publish("sport/tennis", b"one").await.unwrap();
    publisher
        .publish("sport/tennis/player", b"two")
        .await
        .unwrap();

    assert!(
        collector.wait_for_messages(3, TIMEOUT).await,
        "sport/# must match sport, sport/tennis, and sport/tennis/player"
    );

    let msgs = collector.get_messages().await;
    let topics: Vec<&str> = msgs.iter().map(|m| m.topic.as_str()).collect();
    assert!(topics.contains(&"sport"));
    assert!(topics.contains(&"sport/tennis"));
    assert!(topics.contains(&"sport/tennis/player"));
}

// ---------------------------------------------------------------------------
// Group 5: Message Delivery & Ordering — Sections 4.5 & 4.6
// ---------------------------------------------------------------------------

/// `[MQTT-4.5.0-1]` The server MUST deliver published messages to clients
/// that have matching subscriptions.
#[tokio::test]
async fn server_delivers_to_matching_subscribers() {
    let broker = ConformanceBroker::start().await;
    let tag = unique_client_id("deliver");
    let topic = format!("deliver/{tag}");

    let collector_exact = MessageCollector::new();
    let sub_exact = connected_client("sub-exact", &broker).await;
    sub_exact
        .subscribe(&topic, collector_exact.callback())
        .await
        .unwrap();

    let filter_wild = format!("deliver/{tag}/+");
    let collector_wild = MessageCollector::new();
    let sub_wild = connected_client("sub-wild", &broker).await;
    sub_wild
        .subscribe(&filter_wild, collector_wild.callback())
        .await
        .unwrap();

    let collector_non = MessageCollector::new();
    let sub_non = connected_client("sub-non", &broker).await;
    sub_non
        .subscribe("other/topic", collector_non.callback())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("pub-deliver", &broker).await;
    publisher.publish(&topic, b"match-payload").await.unwrap();

    assert!(
        collector_exact.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-4.5.0-1] exact-match subscriber must receive the message"
    );
    let msgs = collector_exact.get_messages().await;
    assert_eq!(msgs[0].payload, b"match-payload");

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        collector_wild.count().await,
        0,
        "wildcard subscriber for deliver/tag/+ must not match deliver/tag"
    );
    assert_eq!(
        collector_non.count().await,
        0,
        "non-matching subscriber must not receive the message"
    );
}

/// `[MQTT-4.6.0-5]` Message ordering MUST be preserved per topic for the
/// same `QoS` level. Send multiple `QoS` 0 messages on the same topic and
/// verify they arrive in order.
#[tokio::test]
async fn message_ordering_preserved_same_qos() {
    let broker = ConformanceBroker::start().await;
    let tag = unique_client_id("order");
    let topic = format!("order/{tag}");

    let collector = MessageCollector::new();
    let subscriber = connected_client("sub-order", &broker).await;
    subscriber
        .subscribe(&topic, collector.callback())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("pub-order", &broker).await;
    for i in 0u32..5 {
        publisher
            .publish(&topic, format!("msg-{i}").into_bytes())
            .await
            .unwrap();
    }

    assert!(
        collector.wait_for_messages(5, TIMEOUT).await,
        "subscriber should receive all 5 messages"
    );

    let msgs = collector.get_messages().await;
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

// ---------------------------------------------------------------------------
// Group 6: Unicode Normalization — Section 4.7.3
// ---------------------------------------------------------------------------

/// `[MQTT-4.7.3-4]` Topic matching MUST NOT apply Unicode normalization.
/// U+00C5 (A-ring precomposed) and U+0041 U+030A (A + combining ring)
/// are visually identical but must be treated as different topics.
#[tokio::test]
async fn topic_matching_no_unicode_normalization() {
    let broker = ConformanceBroker::start().await;
    let tag = unique_client_id("unicode");

    let precomposed = format!("uni/{tag}/\u{00C5}");
    let decomposed = format!("uni/{tag}/A\u{030A}");

    let collector_pre = MessageCollector::new();
    let sub_pre = connected_client("sub-pre", &broker).await;
    sub_pre
        .subscribe(&precomposed, collector_pre.callback())
        .await
        .unwrap();

    let collector_dec = MessageCollector::new();
    let sub_dec = connected_client("sub-dec", &broker).await;
    sub_dec
        .subscribe(&decomposed, collector_dec.callback())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("pub-unicode", &broker).await;
    publisher
        .publish(&precomposed, b"precomposed")
        .await
        .unwrap();

    assert!(
        collector_pre.wait_for_messages(1, TIMEOUT).await,
        "precomposed subscriber must receive message on precomposed topic"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        collector_dec.count().await,
        0,
        "[MQTT-4.7.3-4] decomposed subscriber must NOT receive message on precomposed topic \
         (no Unicode normalization)"
    );

    publisher.publish(&decomposed, b"decomposed").await.unwrap();

    assert!(
        collector_dec.wait_for_messages(1, TIMEOUT).await,
        "decomposed subscriber must receive message on decomposed topic"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    let pre_msgs = collector_pre.get_messages().await;
    assert_eq!(
        pre_msgs.len(),
        1,
        "[MQTT-4.7.3-4] precomposed subscriber must NOT receive message on decomposed topic"
    );
}
