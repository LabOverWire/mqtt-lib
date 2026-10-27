//! Section 3.3 — Advanced PUBLISH features (overlapping subs, `no_local`,
//! message expiry, response topic).

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::packet_parser::ParsedPublish;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use crate::test_client::TestClient;
use mqtt5_protocol::types::{PublishOptions, PublishProperties, QoS, SubscribeOptions};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// `[MQTT-3.3.4-2]` Overlapping subscriptions: the message must be delivered
/// at the maximum granted `QoS` across the matching subscriptions, in one
/// copy or in one copy per subscription.
#[conformance_test(
    ids = ["MQTT-3.3.4-2"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn overlapping_subs_max_qos_delivered(sut: SutHandle) {
    let tag = unique_client_id("overlap");
    let topic = format!("overlap/{tag}/data");
    let filter_plus = format!("overlap/{tag}/+");
    let filter_hash = format!("overlap/{tag}/#");

    let sub_id = unique_client_id("sub-oq");
    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        &filter_plus,
        0,
        1,
    ))
    .await
    .unwrap();
    expect_granted(&mut sub, 0).await;
    sub.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        &filter_hash,
        1,
        2,
    ))
    .await
    .unwrap();
    expect_granted(&mut sub, 1).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub-overlap-qos")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"hello", pub_opts)
        .await
        .expect("publish failed");

    let received = overlapping_copies(&mut sub).await;
    assert!(
        received[0].is_some(),
        "[MQTT-3.3.4-2] overlapping subscriptions must deliver the message"
    );
    assert_eq!(
        received.iter().flatten().map(|publish| publish.qos).max(),
        Some(1),
        "[MQTT-3.3.4-2] the message must be delivered at the maximum QoS of the matching subscriptions"
    );

    publisher.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.4-3]` `[MQTT-3.3.4-4]` `[MQTT-3.3.4-5]` A single copy for
/// overlapping subscriptions must carry every matching Subscription
/// Identifier, and each of several copies must carry its own.
#[conformance_test(
    ids = ["MQTT-3.3.4-3", "MQTT-3.3.4-4", "MQTT-3.3.4-5"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn overlapping_subs_subscription_ids_per_copy(sut: SutHandle) {
    let tag = unique_client_id("subid");
    let topic = format!("subid/{tag}/data");
    let filter_a = format!("subid/{tag}/+");
    let filter_b = format!("subid/{tag}/#");

    let sub_id = unique_client_id("sub-oi");
    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe_with_sub_id(
        &filter_a, 1, 1, 10,
    ))
    .await
    .unwrap();
    expect_granted(&mut sub, 1).await;
    sub.send_raw(&RawPacketBuilder::subscribe_with_sub_id(
        &filter_b, 1, 2, 20,
    ))
    .await
    .unwrap();
    expect_granted(&mut sub, 1).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub-overlap-id")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"identify", pub_opts)
        .await
        .expect("publish failed");

    let received = overlapping_copies(&mut sub).await;
    match &received {
        [Some(single), None] => {
            let mut ids = single.properties.subscription_identifiers.clone();
            ids.sort_unstable();
            assert_eq!(
                ids,
                [10, 20],
                "[MQTT-3.3.4-4] a single copy must carry the Subscription Identifiers of every matching subscription"
            );
        }
        [Some(first), Some(second)] => {
            let mut ids = [
                first.properties.subscription_identifiers.as_slice(),
                second.properties.subscription_identifiers.as_slice(),
            ];
            ids.sort_unstable();
            assert_eq!(
                ids,
                [[10].as_slice(), [20].as_slice()],
                "[MQTT-3.3.4-5] each copy must carry the Subscription Identifier of its matching subscription"
            );
        }
        [None, _] => panic!("overlapping subscriptions must deliver the message"),
    }

    publisher.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.8.3-3]` A subscription with No Local set must not receive
/// messages published by the same client.
#[conformance_test(
    ids = ["MQTT-3.8.3-3"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn overlapping_subs_no_local_prevents_echo(sut: SutHandle) {
    let tag = unique_client_id("nolocal");
    let topic = format!("nolocal/{tag}/data");
    let filter = format!("nolocal/{tag}/+");

    let client = TestClient::connect_with_prefix(&sut, "client-nolocal")
        .await
        .unwrap();

    let sub_opts = SubscribeOptions {
        qos: QoS::AtLeastOnce,
        no_local: true,
        ..Default::default()
    };
    let self_subscription = client
        .subscribe(&filter, sub_opts)
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let other = TestClient::connect_with_prefix(&sut, "other-nolocal")
        .await
        .unwrap();
    let other_sub_opts = SubscribeOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    let other_subscription = other
        .subscribe(&filter, other_sub_opts)
        .await
        .expect("other subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    client
        .publish(&topic, b"echo-test")
        .await
        .expect("publish failed");

    assert!(
        other_subscription.wait_for_messages(1, TIMEOUT).await,
        "other subscriber should receive the message"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        self_subscription.count(),
        0,
        "no_local subscriber must not receive its own publish on wildcard subscription"
    );

    client.disconnect().await.expect("disconnect failed");
    other.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.2-6]` Message Expiry Interval must be decremented by the
/// time the message has been held by the broker.
#[conformance_test(
    ids = ["MQTT-3.3.2-6"],
    requires = ["transport.tcp", "max_qos>=1", "retain_available"],
)]
async fn message_expiry_interval_decremented(sut: SutHandle) {
    let tag = unique_client_id("decrement");
    let topic = format!("decrement/{tag}");

    let publisher = TestClient::connect_with_prefix(&sut, "pub-decrement")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        retain: true,
        properties: PublishProperties {
            message_expiry_interval: Some(30),
            ..Default::default()
        },
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"aging", pub_opts)
        .await
        .expect("publish retained failed");
    publisher.disconnect().await.expect("disconnect failed");

    tokio::time::sleep(Duration::from_secs(2)).await;

    let subscriber = TestClient::connect_with_prefix(&sut, "sub-decrement")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "retained message should be delivered"
    );

    let msgs = subscription.snapshot();
    let expiry = msgs[0]
        .message_expiry_interval
        .expect("[MQTT-3.3.2-6] delivered retained message must include message_expiry_interval");
    assert!(
        expiry < 30,
        "[MQTT-3.3.2-6] message_expiry_interval must be decremented (got {expiry}, expected < 30)"
    );

    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.2-14]` `[MQTT-4.13.1-1]` The Response Topic must not contain
/// wildcards. Sending PUBLISH with a wildcard Response Topic must cause
/// disconnect.
#[conformance_test(
    ids = ["MQTT-3.3.2-14", "MQTT-4.13.1-1"],
    requires = ["transport.tcp"],
)]
async fn response_topic_wildcard_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("rt-wc");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_qos0_with_response_topic(
        "test/topic",
        b"payload",
        "reply/#",
    ))
    .await
    .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.3.2-14] broker must disconnect client that sends PUBLISH with wildcard in Response Topic"
    );
}

/// `[MQTT-3.3.2-15]` The Server MUST send the Response Topic unaltered to
/// subscribers receiving the message.
#[conformance_test(
    ids = ["MQTT-3.3.2-15"],
    requires = ["transport.tcp"],
)]
async fn response_topic_valid_utf8_forwarded(sut: SutHandle) {
    let tag = unique_client_id("rt-fwd");
    let topic = format!("rtfwd/{tag}");

    let subscriber = TestClient::connect_with_prefix(&sut, "sub-rt-fwd")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pub-rt-fwd")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::AtMostOnce,
        properties: PublishProperties {
            response_topic: Some("reply/topic".to_owned()),
            ..Default::default()
        },
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"with-rt", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "subscriber should receive message with response topic"
    );

    let msgs = subscription.snapshot();
    assert_eq!(
        msgs[0].response_topic.as_deref(),
        Some("reply/topic"),
        "[MQTT-3.3.2-15] valid UTF-8 Response Topic must be forwarded to subscriber"
    );

    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

async fn overlapping_copies(sub: &mut RawMqttClient) -> [Option<ParsedPublish>; 2] {
    let first = next_copy(sub).await;
    let second = match first {
        Some(_) => next_copy(sub).await,
        None => None,
    };
    [first, second]
}

async fn next_copy(sub: &mut RawMqttClient) -> Option<ParsedPublish> {
    let packet = sub.read_packet_bytes(TIMEOUT).await?;
    let publish = ParsedPublish::parse(&packet);
    assert!(
        publish.is_some(),
        "overlapping subscriptions must deliver well-formed PUBLISH packets, got {packet:02x?}"
    );
    publish
}

async fn expect_granted(sub: &mut RawMqttClient, qos: u8) {
    let (_, reason_codes) = sub.expect_suback(TIMEOUT).await.expect("expected SUBACK");
    assert_eq!(
        reason_codes,
        [qos],
        "subscription must be granted at QoS {qos}"
    );
}
