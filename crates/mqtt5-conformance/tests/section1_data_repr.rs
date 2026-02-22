use mqtt5_conformance::harness::{unique_client_id, ConformanceBroker};
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// [MQTT-1.5.4-1] The character data in a UTF-8 Encoded String MUST be
/// well-formed UTF-8 as defined by the Unicode specification and restated
/// in RFC 3629. In particular it MUST NOT include encodings of code points
/// between U+D800 and U+DFFF.
///
/// Sends a CONNECT with `client_id` containing UTF-16 surrogate bytes
/// (0xED 0xA0 0x80 = U+D800). Server must disconnect (malformed packet).
#[tokio::test]
async fn utf8_surrogate_codepoints_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
///
/// Subscribes to a topic with BOM prefix and publishes to the same topic,
/// verifying the subscriber receives the message (BOM is preserved, not stripped).
#[tokio::test]
async fn bom_preserved_in_topic() {
    let broker = ConformanceBroker::start().await;
    let tag = unique_client_id("bom");

    let bom_prefix: &[u8] = &[0xEF, 0xBB, 0xBF];
    let topic_suffix = format!("test/{tag}");
    let mut topic_with_bom = Vec::with_capacity(bom_prefix.len() + topic_suffix.len());
    topic_with_bom.extend_from_slice(bom_prefix);
    topic_with_bom.extend_from_slice(topic_suffix.as_bytes());

    let mut subscriber = RawMqttClient::connect_tcp(broker.socket_addr())
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

    let mut publisher = RawMqttClient::connect_tcp(broker.socket_addr())
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

/// [MQTT-1.5.5-1] The encoded value MUST use the minimum number of bytes
/// necessary to represent the value. A variable byte integer MUST NOT use
/// more than 4 bytes.
///
/// Sends a raw CONNECT packet with a 5-byte variable byte integer in the
/// remaining length field (exceeds the 4-byte maximum). Server should
/// disconnect.
#[tokio::test]
async fn non_minimal_varint_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_non_minimal_varint())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-1.5.5-1] server must disconnect on 5-byte variable byte integer"
    );
}

/// [MQTT-1.5.7-1] Both the key and value of a UTF-8 String Pair MUST comply
/// with the requirements for UTF-8 Encoded Strings.
///
/// Sends a PUBLISH with a user property key containing invalid UTF-8 bytes.
/// Server must disconnect (malformed).
#[tokio::test]
async fn invalid_utf8_user_property_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

/// [MQTT-4.7.3-3] Topic Names and Topic Filters are UTF-8 Encoded Strings;
/// they MUST NOT encode to more than 65,535 bytes.
///
/// Sends a PUBLISH with a topic of 65535 'A' characters (the maximum
/// allowed by the 2-byte length prefix). The broker should either accept
/// the packet or cleanly reject it. The connection must not hang or crash.
#[tokio::test]
async fn max_length_topic_handled() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("bigtopic");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_with_oversized_topic())
        .await
        .unwrap();

    let response = raw.expect_any_packet(TIMEOUT).await;
    match response {
        Some(data) if !data.is_empty() && data[0] == 0xE0 => {
            // pass
        }
        Some(_) | None => {
            raw.send_raw(&RawPacketBuilder::pingreq()).await.unwrap();
            let alive = raw.expect_pingresp(TIMEOUT).await;
            assert!(
                alive || raw.expect_disconnect(Duration::from_millis(500)).await,
                "[MQTT-4.7.3-3] broker must handle max-length topic without hanging"
            );
        }
    }
}
