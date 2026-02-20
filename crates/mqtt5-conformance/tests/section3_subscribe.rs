use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::{QoS, SubscribeOptions};
use mqtt5_conformance::harness::{
    connected_client, unique_client_id, ConformanceBroker, MessageCollector,
};
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use std::io::Write;
use std::net::SocketAddr;
use std::time::Duration;
use tempfile::NamedTempFile;

const TIMEOUT: Duration = Duration::from_secs(3);

fn memory_config() -> BrokerConfig {
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config)
}

// ---------------------------------------------------------------------------
// Group 1: SUBSCRIBE Structure — Section 3.8
// ---------------------------------------------------------------------------

/// `[MQTT-3.8.1-1]` SUBSCRIBE fixed header flags MUST be `0x02`.
/// A raw SUBSCRIBE with flags `0x00` (byte `0x80`) must cause disconnect.
#[tokio::test]
async fn subscribe_invalid_flags_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
/// An empty-payload SUBSCRIBE must cause disconnect.
#[tokio::test]
async fn subscribe_empty_payload_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
/// Broker must disconnect.
#[tokio::test]
async fn subscribe_no_local_shared_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

// ---------------------------------------------------------------------------
// Group 2: SUBACK Response — Section 3.9
// ---------------------------------------------------------------------------

/// `[MQTT-3.9.2-1]` SUBACK packet ID must match SUBSCRIBE packet ID.
#[tokio::test]
async fn suback_packet_id_matches() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
#[tokio::test]
async fn suback_reason_codes_per_filter() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

/// `[MQTT-3.9.3-2]` SUBACK reason codes must be in the same order as the
/// SUBSCRIBE topic filters. Mix of authorized and unauthorized filters.
#[tokio::test]
async fn suback_reason_codes_ordering() {
    let mut acl_file = NamedTempFile::new().unwrap();
    writeln!(acl_file, "user * topic allowed/# permission readwrite").unwrap();
    acl_file.flush().unwrap();

    let config = memory_config().with_auth(
        mqtt5::broker::config::AuthConfig::new().with_acl_file(acl_file.path().to_path_buf()),
    );
    let broker = ConformanceBroker::start_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("suback-order");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    let filters = [("allowed/a", 0u8), ("denied/b", 1), ("allowed/c", 2)];
    raw.send_raw(&RawPacketBuilder::subscribe_multiple(&filters, 5))
        .await
        .unwrap();

    let (ack_id, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("expected SUBACK from broker");

    assert_eq!(ack_id, 5);
    assert_eq!(reason_codes.len(), 3, "one reason code per filter");
    assert!(
        reason_codes[0] <= 0x02,
        "first filter (allowed) should succeed, got 0x{:02X}",
        reason_codes[0]
    );
    assert_eq!(
        reason_codes[1], 0x87,
        "second filter (denied) should be NotAuthorized (0x87)"
    );
    assert!(
        reason_codes[2] <= 0x02,
        "third filter (allowed) should succeed, got 0x{:02X}",
        reason_codes[2]
    );
}

// ---------------------------------------------------------------------------
// Group 3: QoS Granting
// ---------------------------------------------------------------------------

/// `[MQTT-3.9.3-3]` SUBACK grants the exact `QoS` requested on a default
/// broker (max `QoS` 2).
#[tokio::test]
async fn suback_grants_requested_qos() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

/// `[MQTT-3.2.2-10]` / `[MQTT-3.9.3-3]` When broker's `maximum_qos=1`,
/// subscribing with `QoS` 2 gets downgraded to `QoS` 1 in SUBACK.
#[tokio::test]
async fn suback_downgrades_to_max_qos() {
    let config = memory_config().with_maximum_qos(1);
    let broker = ConformanceBroker::start_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("suback-downgrade");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/downgrade",
        2,
        1,
    ))
    .await
    .unwrap();

    let (_, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("expected SUBACK from broker");

    assert_eq!(
        reason_codes[0], 0x01,
        "SUBACK should downgrade QoS 2 to QoS 1, got 0x{:02X}",
        reason_codes[0]
    );
}

/// Subscribe `QoS` 1, publish `QoS` 1, verify message delivery to subscriber.
#[tokio::test]
async fn suback_message_delivery_at_granted_qos() {
    let broker = ConformanceBroker::start().await;
    let subscriber = connected_client("sub-deliver", &broker).await;
    let collector = MessageCollector::new();
    let opts = SubscribeOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    subscriber
        .subscribe_with_options("test/sub-deliver", opts, collector.callback())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("sub-deliver-pub", &broker).await;
    publisher
        .publish("test/sub-deliver", b"delivered".to_vec())
        .await
        .unwrap();

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "subscriber should receive message at granted QoS"
    );
    let msgs = collector.get_messages();
    assert_eq!(msgs[0].payload, b"delivered");
}

// ---------------------------------------------------------------------------
// Group 4: Authorization & Quota
// ---------------------------------------------------------------------------

/// SUBACK reason code `NotAuthorized` (0x87) when ACL denies subscribe.
#[tokio::test]
async fn suback_not_authorized() {
    let mut acl_file = NamedTempFile::new().unwrap();
    writeln!(acl_file, "user * topic public/# permission readwrite").unwrap();
    acl_file.flush().unwrap();

    let config = memory_config().with_auth(
        mqtt5::broker::config::AuthConfig::new().with_acl_file(acl_file.path().to_path_buf()),
    );
    let broker = ConformanceBroker::start_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("sub-noauth");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "secret/topic",
        0,
        1,
    ))
    .await
    .unwrap();

    let (_, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("expected SUBACK from broker");

    assert_eq!(
        reason_codes[0], 0x87,
        "SUBACK reason code should be NotAuthorized (0x87), got 0x{:02X}",
        reason_codes[0]
    );
}

/// SUBACK reason code `QuotaExceeded` (0x97) when subscription limit is reached.
#[tokio::test]
async fn suback_quota_exceeded() {
    let config = memory_config().with_max_subscriptions_per_client(2);
    let broker = ConformanceBroker::start_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("sub-quota");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/quota/a",
        0,
        1,
    ))
    .await
    .unwrap();
    let (_, rc1) = raw.expect_suback(TIMEOUT).await.expect("SUBACK 1");
    assert!(rc1[0] <= 0x02, "first subscribe should succeed");

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/quota/b",
        0,
        2,
    ))
    .await
    .unwrap();
    let (_, rc2) = raw.expect_suback(TIMEOUT).await.expect("SUBACK 2");
    assert!(rc2[0] <= 0x02, "second subscribe should succeed");

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/quota/c",
        0,
        3,
    ))
    .await
    .unwrap();
    let (_, rc3) = raw.expect_suback(TIMEOUT).await.expect("SUBACK 3");
    assert_eq!(
        rc3[0], 0x97,
        "third subscribe should be QuotaExceeded (0x97), got 0x{:02X}",
        rc3[0]
    );
}

// ---------------------------------------------------------------------------
// Group 5: Subscription Replacement
// ---------------------------------------------------------------------------

/// Subscribing twice to the same topic with different `QoS` replaces the
/// existing subscription. Only one copy of a published message is delivered.
#[tokio::test]
async fn subscribe_replaces_existing() {
    let broker = ConformanceBroker::start().await;

    let mut raw_sub = RawMqttClient::connect_tcp(broker.socket_addr())
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

    let publisher = connected_client("sub-replace-pub", &broker).await;
    publisher
        .publish("test/replace", b"once".to_vec())
        .await
        .unwrap();

    let first = raw_sub.expect_publish(TIMEOUT).await;
    assert!(first.is_some(), "subscriber should receive the message");

    let duplicate = raw_sub.expect_publish(Duration::from_millis(500)).await;
    assert!(
        duplicate.is_none(),
        "subscriber should receive only one copy (replacement, not duplicate subscription)"
    );
}

// ---------------------------------------------------------------------------
// Group 6: Subscribe Options Validation — Section 3.8.3
// ---------------------------------------------------------------------------

/// `[MQTT-3.8.3-5]` Reserved bits in the subscribe options byte MUST be zero.
/// Setting bits 6-7 to non-zero is a protocol error that must cause disconnect.
#[tokio::test]
async fn subscribe_reserved_option_bits_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
/// Sending invalid UTF-8 bytes must cause disconnect.
#[tokio::test]
async fn subscribe_invalid_utf8_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

// ---------------------------------------------------------------------------
// Group 7: Retain Handling — Section 3.8.4
// ---------------------------------------------------------------------------

/// `[MQTT-3.8.4-4]` When Retain Handling is 0 (`SendAtSubscribe`), retained
/// messages are sent on every subscribe, including re-subscribes.
#[tokio::test]
async fn retain_handling_zero_sends_on_resubscribe() {
    let config = memory_config();
    let broker = ConformanceBroker::start_with_config(config).await;

    let publisher = connected_client("rh0-pub", &broker).await;
    let pub_opts = mqtt5::PublishOptions {
        qos: QoS::AtMostOnce,
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options("test/rh0/topic", b"retained-payload", pub_opts)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let subscriber = connected_client("rh0-sub", &broker).await;
    let collector = MessageCollector::new();
    let sub_opts = SubscribeOptions {
        qos: QoS::AtMostOnce,
        retain_handling: mqtt5::RetainHandling::SendAtSubscribe,
        ..Default::default()
    };
    subscriber
        .subscribe_with_options("test/rh0/topic", sub_opts.clone(), collector.callback())
        .await
        .unwrap();

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "first subscribe with RetainHandling=0 must deliver retained message"
    );
    let msgs = collector.get_messages();
    assert_eq!(msgs[0].payload, b"retained-payload");

    collector.clear();

    subscriber
        .subscribe_with_options("test/rh0/topic", sub_opts, collector.callback())
        .await
        .unwrap();

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.8.4-4] re-subscribe with RetainHandling=0 must deliver retained message again"
    );
    let msgs2 = collector.get_messages();
    assert_eq!(msgs2[0].payload, b"retained-payload");
}

// ---------------------------------------------------------------------------
// Group 8: Delivered QoS — Section 3.8.4
// ---------------------------------------------------------------------------

/// `[MQTT-3.8.4-8]` The delivered `QoS` is the minimum of the published `QoS`
/// and the subscription's granted `QoS`. Subscribe at `QoS` 0, publish at `QoS` 1
/// → delivered at `QoS` 0.
#[tokio::test]
async fn delivered_qos_is_minimum_sub0_pub1() {
    let broker = ConformanceBroker::start().await;
    let tag = unique_client_id("minqos01");
    let topic = format!("minqos/{tag}");

    let mut raw_sub = RawMqttClient::connect_tcp(broker.socket_addr())
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

    let mut raw_publisher = RawMqttClient::connect_tcp(broker.socket_addr())
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

/// `[MQTT-3.8.4-8]` Subscribe at `QoS` 1, publish at `QoS` 2 → delivered at `QoS` 1.
#[tokio::test]
async fn delivered_qos_is_minimum_sub1_pub2() {
    let broker = ConformanceBroker::start().await;
    let tag = unique_client_id("minqos12");
    let topic = format!("minqos/{tag}");

    let mut raw_sub = RawMqttClient::connect_tcp(broker.socket_addr())
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

    let publisher = connected_client("pub-mq12", &broker).await;
    let pub_opts = mqtt5::PublishOptions {
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
