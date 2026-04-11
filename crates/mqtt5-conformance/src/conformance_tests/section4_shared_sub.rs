//! Section 4.8 — Shared Subscriptions.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use crate::test_client::TestClient;
use mqtt5_protocol::types::{PublishOptions, QoS, SubscribeOptions};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// `[MQTT-4.8.2-1]` A valid shared subscription filter
/// (`$share/<name>/<filter>`) must be accepted.
#[conformance_test(
    ids = ["MQTT-4.8.2-1"],
    requires = ["transport.tcp", "shared_subscription_available"],
)]
async fn shared_sub_valid_format_accepted(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("shared-valid");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "$share/mygroup/sensor/+",
        0,
        1,
    ))
    .await
    .unwrap();

    let (_, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("[MQTT-4.8.2-1] must receive SUBACK");
    assert_eq!(reason_codes.len(), 1);
    assert_eq!(
        reason_codes[0], 0x00,
        "[MQTT-4.8.2-1] valid shared subscription must be granted QoS 0"
    );
}

/// `[MQTT-4.8.2-2]` The `ShareName` MUST NOT contain `#` or `+`.
#[conformance_test(
    ids = ["MQTT-4.8.2-2"],
    requires = ["transport.tcp", "shared_subscription_available"],
)]
async fn shared_sub_share_name_with_wildcard_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("shared-badname");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_multiple(
        &[("$share/gr+oup/topic", 0), ("$share/gr#oup/topic", 0)],
        1,
    ))
    .await
    .unwrap();

    let pkt = raw.read_mqtt_packet(TIMEOUT).await;
    match pkt {
        Some(data) if data[0] == 0x90 => {
            let (_, reason_codes) =
                parse_suback_bytes(&data).expect("[MQTT-4.8.2-2] SUBACK must be parseable");
            assert_eq!(reason_codes.len(), 2);
            assert_eq!(
                reason_codes[0], 0x8F,
                "[MQTT-4.8.2-2] ShareName with + must return TopicFilterInvalid (0x8F)"
            );
            assert_eq!(
                reason_codes[1], 0x8F,
                "[MQTT-4.8.2-2] ShareName with # must return TopicFilterInvalid (0x8F)"
            );
        }
        Some(data) if data[0] == 0xE0 => {}
        None => {}
        Some(data) => panic!(
            "[MQTT-4.8.2-2] expected SUBACK (0x90), DISCONNECT (0xE0), or EOF, got {:#04x}",
            data[0]
        ),
    }
}

/// `[MQTT-4.8.2-1]` Incomplete `$share/<name>` without a topic filter after
/// the second `/` must be rejected with `TopicFilterInvalid`.
#[conformance_test(
    ids = ["MQTT-4.8.2-1"],
    requires = ["transport.tcp", "shared_subscription_available"],
)]
async fn shared_sub_incomplete_format_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("shared-incomplete");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "$share/grouponly",
        0,
        1,
    ))
    .await
    .unwrap();

    let pkt = raw.read_mqtt_packet(TIMEOUT).await;
    match pkt {
        Some(data) if data[0] == 0x90 => {
            let (_, reason_codes) =
                parse_suback_bytes(&data).expect("[MQTT-4.8.2-1] SUBACK must be parseable");
            assert_eq!(reason_codes.len(), 1);
            assert_eq!(
                reason_codes[0], 0x8F,
                "[MQTT-4.8.2-1] incomplete $share/grouponly must return TopicFilterInvalid"
            );
        }
        Some(data) if data[0] == 0xE0 => {}
        None => {}
        Some(data) => panic!(
            "[MQTT-4.8.2-1] expected SUBACK (0x90), DISCONNECT (0xE0), or EOF, got {:#04x}",
            data[0]
        ),
    }
}

/// `[MQTT-4.8.2-4]` Messages published to a shared subscription are
/// distributed across the members of the shared group.
#[conformance_test(
    ids = ["MQTT-4.8.2-4"],
    requires = ["transport.tcp", "shared_subscription_available"],
)]
async fn shared_sub_round_robin_delivery(sut: SutHandle) {
    let topic = format!("tasks/{}", unique_client_id("rr"));
    let shared_filter = format!("$share/workers/{topic}");

    let worker1 = TestClient::connect_with_prefix(&sut, "shared-rr-w1")
        .await
        .unwrap();
    let sub1 = worker1
        .subscribe(&shared_filter, SubscribeOptions::default())
        .await
        .unwrap();

    let worker2 = TestClient::connect_with_prefix(&sut, "shared-rr-w2")
        .await
        .unwrap();
    let sub2 = worker2
        .subscribe(&shared_filter, SubscribeOptions::default())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "shared-rr-pub")
        .await
        .unwrap();
    for i in 0..6 {
        let payload = format!("msg-{i}");
        publisher.publish(&topic, payload.as_bytes()).await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let count1 = sub1.count();
    let count2 = sub2.count();
    assert_eq!(
        count1 + count2,
        6,
        "all 6 messages must be delivered across shared group"
    );
    assert!(
        count1 >= 1 && count2 >= 1,
        "both shared subscribers must receive at least one message: w1={count1}, w2={count2}"
    );
}

/// `[MQTT-4.8.2-4]` A shared subscription coexists with regular
/// subscriptions on the same topic; both must receive the message.
#[conformance_test(
    ids = ["MQTT-4.8.2-4"],
    requires = ["transport.tcp", "shared_subscription_available"],
)]
async fn shared_sub_mixed_with_regular(sut: SutHandle) {
    let topic = format!("tasks/{}", unique_client_id("mix"));
    let shared_filter = format!("$share/workers/{topic}");

    let shared = TestClient::connect_with_prefix(&sut, "shared-mix-s")
        .await
        .unwrap();
    let sub_shared = shared
        .subscribe(&shared_filter, SubscribeOptions::default())
        .await
        .unwrap();

    let regular = TestClient::connect_with_prefix(&sut, "shared-mix-r")
        .await
        .unwrap();
    let sub_regular = regular
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "shared-mix-pub")
        .await
        .unwrap();
    for i in 0..4 {
        let payload = format!("msg-{i}");
        publisher.publish(&topic, payload.as_bytes()).await.unwrap();
    }

    sub_regular.wait_for_messages(4, TIMEOUT).await;
    sub_shared.wait_for_messages(4, TIMEOUT).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        sub_regular.count(),
        4,
        "regular subscriber must receive all 4 messages"
    );
    assert_eq!(
        sub_shared.count(),
        4,
        "sole shared group member must receive all 4 messages"
    );
}

/// `[MQTT-4.8.2-5]` Shared subscriptions MUST NOT receive retained messages
/// on subscribe.
#[conformance_test(
    ids = ["MQTT-4.8.2-5"],
    requires = ["transport.tcp", "shared_subscription_available", "retain_available"],
)]
async fn shared_sub_no_retained_on_subscribe(sut: SutHandle) {
    let topic = format!("sensor/temp/{}", unique_client_id("ret"));
    let shared_filter = format!("$share/readers/{topic}");

    let publisher = TestClient::connect_with_prefix(&sut, "shared-ret-pub")
        .await
        .unwrap();
    publisher
        .publish_with_options(
            &topic,
            b"25.5",
            PublishOptions {
                retain: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let shared_sub = TestClient::connect_with_prefix(&sut, "shared-ret-s")
        .await
        .unwrap();
    let sub_shared = shared_sub
        .subscribe(&shared_filter, SubscribeOptions::default())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        sub_shared.count(),
        0,
        "shared subscription must not receive retained messages on subscribe"
    );

    let regular_sub = TestClient::connect_with_prefix(&sut, "shared-ret-r")
        .await
        .unwrap();
    let sub_regular = regular_sub
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .unwrap();

    sub_regular.wait_for_messages(1, TIMEOUT).await;
    let msgs = sub_regular.snapshot();
    assert_eq!(
        msgs.len(),
        1,
        "regular subscription must receive retained message"
    );
    assert_eq!(msgs[0].payload, b"25.5");
}

/// `[MQTT-4.8.2-4]` After unsubscribing from a shared subscription, the
/// client must no longer receive messages distributed to the share group.
#[conformance_test(
    ids = ["MQTT-4.8.2-4"],
    requires = ["transport.tcp", "shared_subscription_available"],
)]
async fn shared_sub_unsubscribe_stops_delivery(sut: SutHandle) {
    let topic = format!("tasks/{}", unique_client_id("unsub"));
    let shared_filter = format!("$share/workers/{topic}");

    let subscriber = TestClient::connect_with_prefix(&sut, "shared-unsub-s")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&shared_filter, SubscribeOptions::default())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "shared-unsub-pub")
        .await
        .unwrap();
    publisher.publish(&topic, b"before").await.unwrap();

    subscription.wait_for_messages(1, TIMEOUT).await;
    assert_eq!(
        subscription.count(),
        1,
        "must receive message before unsubscribe"
    );

    subscriber.unsubscribe(&shared_filter).await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    publisher.publish(&topic, b"after").await.unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        subscription.count(),
        1,
        "must not receive messages after unsubscribe from shared subscription"
    );
}

/// `[MQTT-4.8.2-4]` Multiple shared groups with different `ShareNames` on the
/// same topic are independent; each group receives the message.
#[conformance_test(
    ids = ["MQTT-4.8.2-4"],
    requires = ["transport.tcp", "shared_subscription_available"],
)]
async fn shared_sub_multiple_groups_independent(sut: SutHandle) {
    let topic = format!("topic/{}", unique_client_id("grp"));
    let filter_a = format!("$share/groupA/{topic}");
    let filter_b = format!("$share/groupB/{topic}");

    let client_a = TestClient::connect_with_prefix(&sut, "shared-grpA")
        .await
        .unwrap();
    let sub_a = client_a
        .subscribe(&filter_a, SubscribeOptions::default())
        .await
        .unwrap();

    let client_b = TestClient::connect_with_prefix(&sut, "shared-grpB")
        .await
        .unwrap();
    let sub_b = client_b
        .subscribe(&filter_b, SubscribeOptions::default())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "shared-grp-pub")
        .await
        .unwrap();
    publisher.publish(&topic, b"payload").await.unwrap();

    sub_a.wait_for_messages(1, TIMEOUT).await;
    sub_b.wait_for_messages(1, TIMEOUT).await;

    assert_eq!(
        sub_a.count(),
        1,
        "groupA must receive the message independently"
    );
    assert_eq!(
        sub_b.count(),
        1,
        "groupB must receive the message independently"
    );
}

/// `[MQTT-4.8.2-3]` Messages delivered to a shared subscription are
/// delivered at the minimum of the publish `QoS` and the granted `QoS`.
#[conformance_test(
    ids = ["MQTT-4.8.2-3"],
    requires = ["transport.tcp", "shared_subscription_available", "max_qos>=1"],
)]
async fn shared_sub_respects_granted_qos(sut: SutHandle) {
    let topic = format!("shared-qos/{}", unique_client_id("t"));
    let shared_filter = format!("$share/qgrp/{topic}");

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub-sqos");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;

    sub.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        &shared_filter,
        0,
        1,
    ))
    .await
    .unwrap();
    let (_, reason_codes) = sub
        .expect_suback(TIMEOUT)
        .await
        .expect("must receive SUBACK");
    assert_eq!(
        reason_codes[0], 0x00,
        "shared subscription must be granted at QoS 0"
    );

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut pub_raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("pub-sqos");
    pub_raw.connect_and_establish(&pub_id, TIMEOUT).await;

    pub_raw
        .send_raw(&RawPacketBuilder::publish_qos1(&topic, b"qos1-data", 1))
        .await
        .unwrap();
    let _ = pub_raw.expect_puback(TIMEOUT).await;

    let (qos, recv_topic, payload) = sub
        .expect_publish(TIMEOUT)
        .await
        .expect("[MQTT-4.8.2-3] subscriber must receive message");

    assert_eq!(recv_topic, topic);
    assert_eq!(payload, b"qos1-data");
    assert_eq!(
        qos, 0,
        "[MQTT-4.8.2-3] message published at QoS 1 must be downgraded to granted QoS 0"
    );
}

/// `[MQTT-4.8.2-6]` A message rejected by a shared subscriber with a
/// PUBACK error reason code MUST NOT be redistributed to other members of
/// the share group.
#[conformance_test(
    ids = ["MQTT-4.8.2-6"],
    requires = ["transport.tcp", "shared_subscription_available", "max_qos>=1"],
)]
async fn shared_sub_puback_error_discards(sut: SutHandle) {
    let topic = format!("shared-err/{}", unique_client_id("t"));
    let shared_filter = format!("$share/errgrp/{topic}");

    let mut worker = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let worker_id = unique_client_id("shared-err-w");
    worker.connect_and_establish(&worker_id, TIMEOUT).await;
    worker
        .send_raw(&RawPacketBuilder::subscribe_with_packet_id(
            &shared_filter,
            1,
            1,
        ))
        .await
        .unwrap();
    let _ = worker.expect_suback(TIMEOUT).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "shared-err-pub")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"reject-me", pub_opts)
        .await
        .unwrap();

    let received = worker
        .expect_publish_with_id(TIMEOUT)
        .await
        .expect("sole shared subscriber must receive the message");

    let (_, packet_id, _, _, _) = received;
    worker
        .send_raw(&RawPacketBuilder::puback_with_reason(packet_id, 0x80))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let redistributed = worker.read_packet_bytes(Duration::from_secs(1)).await;
    assert!(
        redistributed.is_none(),
        "[MQTT-4.8.2-6] message rejected with PUBACK error must not be redistributed"
    );
}

fn parse_suback_bytes(data: &[u8]) -> Option<(u16, Vec<u8>)> {
    if data.len() < 4 || data[0] != 0x90 {
        return None;
    }
    let mut idx = 1;
    let mut remaining_len: u32 = 0;
    let mut shift = 0;
    loop {
        if idx >= data.len() {
            return None;
        }
        let byte = data[idx];
        idx += 1;
        remaining_len |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 21 {
            return None;
        }
    }
    let payload_start = idx;
    if data.len() < payload_start + remaining_len as usize || remaining_len < 3 {
        return None;
    }
    let packet_id = u16::from_be_bytes([data[idx], data[idx + 1]]);
    idx += 2;
    let mut props_len: u32 = 0;
    let mut props_shift = 0;
    loop {
        if idx >= data.len() {
            return None;
        }
        let byte = data[idx];
        idx += 1;
        props_len |= u32::from(byte & 0x7F) << props_shift;
        if byte & 0x80 == 0 {
            break;
        }
        props_shift += 7;
        if props_shift > 21 {
            return None;
        }
    }
    idx += props_len as usize;
    let end = payload_start + remaining_len as usize;
    if idx > end {
        return None;
    }
    Some((packet_id, data[idx..end].to_vec()))
}
