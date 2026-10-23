//! Section 1.5 — Data Representation: UTF-8 strings, variable byte integers.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// [MQTT-1.5.4-1] [MQTT-3.1.4-1] UTF-8 Encoded Strings MUST NOT include
/// encodings of code points between U+D800 and U+DFFF. A CONNECT carrying one
/// does not match the format of section 3.1, so the Server must close the
/// connection.
#[conformance_test(
    ids = ["MQTT-1.5.4-1", "MQTT-3.1.4-1"],
    requires = ["transport.tcp"],
)]
async fn utf8_surrogate_codepoints_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_surrogate_utf8())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-1.5.4-1] server must disconnect when client_id contains UTF-16 surrogate codepoint"
    );
}

/// [MQTT-1.5.4-3] A UTF-8 Encoded String MUST NOT include an encoding of the
/// null character U+0000. A Byte Order Mark (BOM, U+FEFF) is valid and MUST NOT
/// be stripped by the server.
#[conformance_test(
    ids = ["MQTT-1.5.4-3"],
    requires = ["transport.tcp"],
)]
async fn bom_preserved_in_topic(sut: SutHandle) {
    let tag = unique_client_id("bom");

    let bom_prefix: &[u8] = &[0xEF, 0xBB, 0xBF];
    let topic_suffix = format!("test/{tag}");
    let mut topic_with_bom = Vec::with_capacity(bom_prefix.len() + topic_suffix.len());
    topic_with_bom.extend_from_slice(bom_prefix);
    topic_with_bom.extend_from_slice(topic_suffix.as_bytes());

    let mut subscriber = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("bom-sub");
    subscriber.connect_and_establish(&sub_id, TIMEOUT).await;

    subscriber
        .send_raw(&RawPacketBuilder::subscribe_raw_topic(
            &topic_with_bom,
            0,
            1,
        ))
        .await
        .unwrap();
    let suback = subscriber.expect_suback(TIMEOUT).await;
    assert!(suback.is_some(), "must receive SUBACK");
    let (_, reason_codes) = suback.unwrap();
    assert_eq!(
        reason_codes[0], 0x00,
        "subscription with BOM topic must be accepted"
    );

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut publisher = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("bom-pub");
    publisher.connect_and_establish(&pub_id, TIMEOUT).await;

    publisher
        .send_raw(&RawPacketBuilder::publish_qos0_raw_topic(
            &topic_with_bom,
            b"bom-payload",
        ))
        .await
        .unwrap();

    let received = subscriber.expect_publish(TIMEOUT).await;
    assert!(
        received.is_some(),
        "[MQTT-1.5.4-3] subscriber on BOM-prefixed topic must receive publish on same BOM topic"
    );
    let (_, topic, payload) = received.unwrap();
    assert_eq!(payload, b"bom-payload");
    assert!(
        topic.as_bytes().starts_with(bom_prefix),
        "[MQTT-1.5.4-3] BOM must be preserved in delivered topic name"
    );
}

/// [MQTT-3.1.4-1] A Remaining Length encoded in five bytes is not a valid
/// Variable Byte Integer, which has at most four, so the CONNECT does not match
/// the format of section 3.1 and the Server must close the connection.
#[conformance_test(
    ids = ["MQTT-3.1.4-1"],
    requires = ["transport.tcp"],
)]
async fn non_minimal_varint_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_non_minimal_varint())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.1.4-1] server must disconnect on 5-byte variable byte integer"
    );
}

/// [MQTT-1.5.7-1] [MQTT-4.13.1-1] Both the key and the value of a UTF-8 String
/// Pair MUST be UTF-8 Encoded Strings. A PUBLISH whose User Property key is
/// invalid UTF-8 is a Malformed Packet, so the Server must close the
/// connection.
#[conformance_test(
    ids = ["MQTT-1.5.7-1", "MQTT-4.13.1-1"],
    requires = ["transport.tcp"],
)]
async fn invalid_utf8_user_property_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("utf8prop");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    let topic = format!("utf8prop/{client_id}");
    raw.send_raw(&RawPacketBuilder::publish_with_invalid_utf8_user_property(
        &topic, b"payload",
    ))
    .await
    .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-1.5.7-1] server must disconnect when user property key contains invalid UTF-8"
    );
}
