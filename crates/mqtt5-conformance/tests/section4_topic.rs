#![allow(clippy::cast_possible_truncation)]

mod common;

use common::{
    connected_client, unique_client_id, ConformanceBroker, MessageCollector, RawMqttClient,
    RawPacketBuilder,
};
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
