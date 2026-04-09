//! Section 3.3 — PUBLISH Topic Alias and DUP flag.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// `[MQTT-3.3.2-12]` A sender can modify the Topic Alias mapping by
/// sending another PUBLISH with the same Topic Alias value and a
/// different topic.
#[conformance_test(
    ids = ["MQTT-3.3.2-12"],
    requires = ["transport.tcp"],
)]
async fn topic_alias_register_and_reuse(sut: SutHandle) {
    let topic = format!("alias/{}", unique_client_id("reg"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("asub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 0))
        .await
        .unwrap();
    sub.expect_suback(TIMEOUT).await;

    let mut pub_client = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("apub");
    pub_client.connect_and_establish(&pub_id, TIMEOUT).await;

    pub_client
        .send_raw(&RawPacketBuilder::publish_qos0_with_topic_alias(
            &topic, b"first", 1,
        ))
        .await
        .unwrap();

    let msg1 = sub.expect_publish(TIMEOUT).await;
    assert!(
        msg1.is_some(),
        "Subscriber must receive first message (alias registration)"
    );
    let (_, recv_topic, recv_payload) = msg1.unwrap();
    assert_eq!(recv_topic, topic, "First message topic must match");
    assert_eq!(recv_payload, b"first");

    pub_client
        .send_raw(&RawPacketBuilder::publish_qos0_alias_only(b"second", 1))
        .await
        .unwrap();

    let msg2 = sub.expect_publish(TIMEOUT).await;
    assert!(
        msg2.is_some(),
        "[MQTT-3.3.2-12] Subscriber must receive second message (alias reuse)"
    );
    let (_, recv_topic2, recv_payload2) = msg2.unwrap();
    assert_eq!(
        recv_topic2, topic,
        "[MQTT-3.3.2-12] Reused alias must resolve to original topic"
    );
    assert_eq!(recv_payload2, b"second");
}

/// `[MQTT-3.3.2-12]` A sender can modify the Topic Alias mapping by
/// sending another PUBLISH with the same Topic Alias value and a
/// different non-zero length Topic Name.
#[conformance_test(
    ids = ["MQTT-3.3.2-12"],
    requires = ["transport.tcp"],
)]
async fn topic_alias_update_mapping(sut: SutHandle) {
    let prefix = unique_client_id("remap");
    let topic_a = format!("alias/{prefix}/a");
    let topic_b = format!("alias/{prefix}/b");
    let filter = format!("alias/{prefix}/+");

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("rsub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&filter, 0))
        .await
        .unwrap();
    sub.expect_suback(TIMEOUT).await;

    let mut pub_client = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("rpub");
    pub_client.connect_and_establish(&pub_id, TIMEOUT).await;

    pub_client
        .send_raw(&RawPacketBuilder::publish_qos0_with_topic_alias(
            &topic_a, b"msg-a", 5,
        ))
        .await
        .unwrap();
    let first = sub.expect_publish(TIMEOUT).await.unwrap();
    assert_eq!(first.1, topic_a);

    pub_client
        .send_raw(&RawPacketBuilder::publish_qos0_with_topic_alias(
            &topic_b, b"msg-b", 5,
        ))
        .await
        .unwrap();
    let second = sub.expect_publish(TIMEOUT).await.unwrap();
    assert_eq!(second.1, topic_b);

    pub_client
        .send_raw(&RawPacketBuilder::publish_qos0_alias_only(b"msg-reuse", 5))
        .await
        .unwrap();
    let third = sub.expect_publish(TIMEOUT).await;
    assert!(
        third.is_some(),
        "[MQTT-3.3.2-12] Alias reuse after remap must deliver"
    );
    assert_eq!(
        third.unwrap().1,
        topic_b,
        "[MQTT-3.3.2-12] Remapped alias must resolve to new topic"
    );
}

/// `[MQTT-3.3.2-10]` `[MQTT-3.3.2-11]` Topic Alias mappings are scoped to
/// the Network Connection. A new connection starts with no mappings.
#[conformance_test(
    ids = ["MQTT-3.3.2-10", "MQTT-3.3.2-11"],
    requires = ["transport.tcp"],
)]
async fn topic_alias_not_shared_across_connections(sut: SutHandle) {
    let topic = format!("alias/{}", unique_client_id("scope"));

    let mut conn_a = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let id_a = unique_client_id("sca");
    conn_a.connect_and_establish(&id_a, TIMEOUT).await;
    conn_a
        .send_raw(&RawPacketBuilder::publish_qos0_with_topic_alias(
            &topic, b"setup", 1,
        ))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut conn_b = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let id_b = unique_client_id("scb");
    conn_b.connect_and_establish(&id_b, TIMEOUT).await;
    conn_b
        .send_raw(&RawPacketBuilder::publish_qos0_alias_only(b"bad", 1))
        .await
        .unwrap();

    assert!(
        conn_b.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.3.2-10/11] Alias from connection A must not be usable on connection B"
    );
}

/// `[MQTT-3.3.2-10]` `[MQTT-3.3.2-11]` Topic Alias mappings do not survive
/// reconnection.
#[conformance_test(
    ids = ["MQTT-3.3.2-10", "MQTT-3.3.2-11"],
    requires = ["transport.tcp"],
)]
async fn topic_alias_cleared_on_reconnect(sut: SutHandle) {
    let topic = format!("alias/{}", unique_client_id("recon"));
    let client_id = unique_client_id("rcid");

    let mut conn1 = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    conn1.connect_and_establish(&client_id, TIMEOUT).await;
    conn1
        .send_raw(&RawPacketBuilder::publish_qos0_with_topic_alias(
            &topic, b"setup", 3,
        ))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    conn1
        .send_raw(&RawPacketBuilder::disconnect_normal())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut conn2 = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    conn2.connect_and_establish(&client_id, TIMEOUT).await;
    conn2
        .send_raw(&RawPacketBuilder::publish_qos0_alias_only(b"bad", 3))
        .await
        .unwrap();

    assert!(
        conn2.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.3.2-10/11] Alias from previous connection must not survive reconnection"
    );
}

/// `[MQTT-3.3.2-8]` Topic Alias is stripped before delivery to subscribers.
#[conformance_test(
    ids = ["MQTT-3.3.2-8"],
    requires = ["transport.tcp"],
)]
async fn topic_alias_stripped_before_delivery(sut: SutHandle) {
    let topic = format!("alias/{}", unique_client_id("strip"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("ssub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 0))
        .await
        .unwrap();
    sub.expect_suback(TIMEOUT).await;

    let mut pub_client = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("spub");
    pub_client.connect_and_establish(&pub_id, TIMEOUT).await;
    pub_client
        .send_raw(&RawPacketBuilder::publish_qos0_with_topic_alias(
            &topic, b"aliased", 2,
        ))
        .await
        .unwrap();

    let msg = sub.expect_publish(TIMEOUT).await;
    assert!(msg.is_some(), "Subscriber must receive the message");
    let (_, recv_topic, _) = msg.unwrap();
    assert!(
        !recv_topic.is_empty(),
        "Subscriber must receive non-empty topic name, not alias reference"
    );
    assert_eq!(
        recv_topic, topic,
        "Subscriber must receive the full topic name"
    );
}

/// `[MQTT-3.3.1-3]` The value of the DUP flag from an incoming PUBLISH
/// packet is not propagated when the PUBLISH packet is sent to
/// subscribers.
#[conformance_test(
    ids = ["MQTT-3.3.1-3"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn dup_flag_not_propagated_to_subscribers(sut: SutHandle) {
    let topic = format!("dup/{}", unique_client_id("prop"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("dsub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 1))
        .await
        .unwrap();
    sub.expect_suback(TIMEOUT).await;

    let mut pub_client = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("dpub");
    pub_client.connect_and_establish(&pub_id, TIMEOUT).await;

    pub_client
        .send_raw(&RawPacketBuilder::publish_qos1_with_dup(
            &topic,
            b"dup-test",
            1,
        ))
        .await
        .unwrap();
    pub_client.expect_puback(TIMEOUT).await;

    let msg = sub.expect_publish_raw_header(TIMEOUT).await;
    assert!(
        msg.is_some(),
        "Subscriber must receive the forwarded PUBLISH"
    );
    let (first_byte, _, recv_topic, recv_payload) = msg.unwrap();
    assert_eq!(recv_topic, topic);
    assert_eq!(recv_payload, b"dup-test");
    let dup_bit = (first_byte >> 3) & 0x01;
    assert_eq!(
        dup_bit, 0,
        "[MQTT-3.3.1-3] DUP flag must not be propagated to subscribers (first_byte={first_byte:#04x})"
    );
}
