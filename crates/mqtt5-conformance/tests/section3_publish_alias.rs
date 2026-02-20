mod common;

use common::{unique_client_id, ConformanceBroker, RawMqttClient, RawPacketBuilder};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Group 1: Topic Alias Lifecycle
// ---------------------------------------------------------------------------

/// `[MQTT-3.3.2-12]` A sender can modify the Topic Alias mapping by sending
/// another PUBLISH with the same Topic Alias value and a different topic.
///
/// Registers alias 1 → topic A via PUBLISH with both topic+alias, then reuses
/// alias 1 via PUBLISH with empty topic+alias. Subscriber receives both
/// messages with the correct topic.
#[tokio::test]
async fn topic_alias_register_and_reuse() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("alias/{}", unique_client_id("reg"));

    let mut sub = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("asub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 0))
        .await
        .unwrap();
    sub.expect_suback(TIMEOUT).await;

    let mut pub_client = RawMqttClient::connect_tcp(broker.socket_addr())
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

/// `[MQTT-3.3.2-12]` A sender can modify the Topic Alias mapping by sending
/// another PUBLISH with the same Topic Alias value and a different non-zero
/// length Topic Name.
///
/// Registers alias 5 = topic A, remaps alias 5 = topic B, then reuses alias 5.
/// The reused alias resolves to topic B.
#[tokio::test]
async fn topic_alias_update_mapping() {
    let broker = ConformanceBroker::start().await;
    let prefix = unique_client_id("remap");
    let topic_a = format!("alias/{prefix}/a");
    let topic_b = format!("alias/{prefix}/b");
    let filter = format!("alias/{prefix}/+");

    let mut sub = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("rsub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&filter, 0))
        .await
        .unwrap();
    sub.expect_suback(TIMEOUT).await;

    let mut pub_client = RawMqttClient::connect_tcp(broker.socket_addr())
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

/// `[MQTT-3.3.2-10]` `[MQTT-3.3.2-11]` Topic Alias mappings are scoped to the
/// Network Connection. A new connection starts with no mappings.
///
/// Registers alias 1 on connection A. Opens connection B and tries an
/// alias-only PUBLISH with alias 1 — broker disconnects because alias 1 is
/// not registered on connection B.
#[tokio::test]
async fn topic_alias_not_shared_across_connections() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("alias/{}", unique_client_id("scope"));

    let mut conn_a = RawMqttClient::connect_tcp(broker.socket_addr())
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

    let mut conn_b = RawMqttClient::connect_tcp(broker.socket_addr())
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
///
/// Registers alias 3 on a connection, disconnects normally, reconnects with
/// the same client ID, then tries alias-only PUBLISH with alias 3 — broker
/// disconnects because alias mappings were cleared.
#[tokio::test]
async fn topic_alias_cleared_on_reconnect() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("alias/{}", unique_client_id("recon"));
    let client_id = unique_client_id("rcid");

    let mut conn1 = RawMqttClient::connect_tcp(broker.socket_addr())
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

    let mut conn2 = RawMqttClient::connect_tcp(broker.socket_addr())
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

/// Topic Alias is stripped before delivery to subscribers.
///
/// Publishes with a Topic Alias. The subscriber must receive the full topic
/// name (not an empty string).
#[tokio::test]
async fn topic_alias_stripped_before_delivery() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("alias/{}", unique_client_id("strip"));

    let mut sub = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("ssub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 0))
        .await
        .unwrap();
    sub.expect_suback(TIMEOUT).await;

    let mut pub_client = RawMqttClient::connect_tcp(broker.socket_addr())
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

// ---------------------------------------------------------------------------
// Group 2: DUP Flag
// ---------------------------------------------------------------------------

/// `[MQTT-3.3.1-3]` The value of the DUP flag from an incoming PUBLISH packet
/// is not propagated when the PUBLISH packet is sent to subscribers.
///
/// Raw publisher sends a `QoS` 1 PUBLISH with DUP=1 (header byte `0x3A`). Raw
/// subscriber receives the forwarded PUBLISH. The DUP bit (bit 3 of the first
/// byte) MUST be 0 in the forwarded copy.
#[tokio::test]
async fn dup_flag_not_propagated_to_subscribers() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("dup/{}", unique_client_id("prop"));

    let mut sub = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("dsub");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 1))
        .await
        .unwrap();
    sub.expect_suback(TIMEOUT).await;

    let mut pub_client = RawMqttClient::connect_tcp(broker.socket_addr())
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
