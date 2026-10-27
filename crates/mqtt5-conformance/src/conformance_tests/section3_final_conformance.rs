//! Section 3 — Final conformance tests for UTF-8 validation, server
//! DISCONNECT ordering, request-problem-information suppression, and `QoS` 2
//! message expiry behaviour.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::packet_parser::{decode_variable_int, ParsedProperties};
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// `[MQTT-3.1.3-11]` `[MQTT-3.1.4-1]` The Will Topic MUST be a UTF-8 Encoded
/// String. A CONNECT whose Will Topic is invalid UTF-8 does not match the
/// format of section 3.1, so the Server must close the connection.
#[conformance_test(
    ids = ["MQTT-3.1.3-11", "MQTT-3.1.4-1"],
    requires = ["transport.tcp"],
)]
async fn will_topic_invalid_utf8_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("will-utf8");

    raw.send_raw(&RawPacketBuilder::connect_with_invalid_utf8_will_topic(
        &client_id,
    ))
    .await
    .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.1.3-11] server must reject CONNECT with invalid UTF-8 Will Topic"
    );
}

/// `[MQTT-3.1.3-12]` `[MQTT-3.1.4-1]` The User Name MUST be a UTF-8 Encoded
/// String. A CONNECT whose User Name is invalid UTF-8 does not match the format
/// of section 3.1, so the Server must close the connection.
#[conformance_test(
    ids = ["MQTT-3.1.3-12", "MQTT-3.1.4-1"],
    requires = ["transport.tcp"],
)]
async fn username_invalid_utf8_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("user-utf8");

    raw.send_raw(&RawPacketBuilder::connect_with_invalid_utf8_username(
        &client_id,
    ))
    .await
    .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.1.3-12] server must reject CONNECT with invalid UTF-8 Username"
    );
}

/// `[MQTT-3.14.0-1]` Server MUST NOT send DISCONNECT until after it has sent a
/// CONNACK with Reason Code < 0x80.
#[conformance_test(
    ids = ["MQTT-3.14.0-1"],
    requires = ["transport.tcp"],
)]
async fn no_server_disconnect_before_connack(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_protocol_version(99))
        .await
        .unwrap();

    if let Some(bytes) = raw.read_packet_bytes(TIMEOUT).await {
        assert_ne!(
            bytes[0], 0xE0,
            "[MQTT-3.14.0-1] first packet from server must not be DISCONNECT (0xE0), got 0x{:02X}",
            bytes[0]
        );
        assert_eq!(
            bytes[0], 0x20,
            "expected CONNACK (0x20), got 0x{:02X}",
            bytes[0]
        );
    }
}

/// `[MQTT-3.14.2-2]` Session Expiry Interval MUST NOT be sent on a server
/// DISCONNECT.
#[conformance_test(
    ids = ["MQTT-3.14.2-2"],
    requires = ["transport.tcp"],
)]
async fn server_disconnect_no_session_expiry(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("disc-noexp");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    if let Some(bytes) = raw.expect_disconnect_raw(TIMEOUT).await {
        let (remaining_len, body_start) = decode_variable_int(&bytes, 1)
            .expect("server DISCONNECT has a malformed Remaining Length");
        if remaining_len >= 2 {
            let (props_len, props_start) = decode_variable_int(&bytes, body_start + 1)
                .expect("server DISCONNECT has a malformed properties length");
            let props = ParsedProperties::parse(&bytes, props_start, props_len as usize)
                .expect("server DISCONNECT has malformed properties");
            assert!(
                props.session_expiry_interval.is_none(),
                "[MQTT-3.14.2-2] server DISCONNECT must not contain Session Expiry Interval (0x11)"
            );
        }
    }
}

/// `[MQTT-3.1.2-29]` If Request Problem Information is 0, the Server MUST NOT
/// send a Reason String or User Property on any packet other than PUBLISH,
/// CONNACK, or DISCONNECT.
#[conformance_test(
    ids = ["MQTT-3.1.2-29"],
    requires = ["transport.tcp"],
)]
async fn request_problem_info_zero_suppresses_properties(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("rpi-zero");

    raw.send_raw(&RawPacketBuilder::connect_with_request_problem_info(
        &client_id, 0,
    ))
    .await
    .unwrap();
    raw.expect_connack(TIMEOUT).await.expect("expected CONNACK");

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/rpi-zero",
        0,
        1,
    ))
    .await
    .unwrap();

    let data = raw
        .read_packet_bytes(TIMEOUT)
        .await
        .expect("expected SUBACK");
    assert_eq!(data[0], 0x90, "expected SUBACK packet type");

    let (_, body_start) =
        decode_variable_int(&data, 1).expect("SUBACK has a malformed Remaining Length");
    let (props_len, props_start) = decode_variable_int(&data, body_start + 2)
        .expect("SUBACK has a malformed properties length");
    let props = ParsedProperties::parse(&data, props_start, props_len as usize)
        .expect("SUBACK has malformed properties");
    assert!(
        props.reason_string.is_none(),
        "[MQTT-3.1.2-29] SUBACK must not contain Reason String when Request Problem Information=0"
    );
    assert!(
        props.user_properties.is_empty(),
        "[MQTT-3.1.2-29] SUBACK must not contain User Property when Request Problem Information=0"
    );
}

/// `[MQTT-4.3.3-7]` Server MUST NOT apply message expiry if PUBLISH has been
/// sent.
#[conformance_test(
    ids = ["MQTT-4.3.3-7"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn qos2_no_message_expiry_after_publish_sent(sut: SutHandle) {
    let mut sub_raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("exp-sub");
    sub_raw.connect_and_establish(&sub_id, TIMEOUT).await;

    sub_raw
        .send_raw(&RawPacketBuilder::subscribe_with_packet_id(
            "test/expiry-sent",
            2,
            1,
        ))
        .await
        .unwrap();
    sub_raw
        .expect_suback(TIMEOUT)
        .await
        .expect("expected SUBACK");

    let mut pub_raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("exp-pub");
    pub_raw.connect_and_establish(&pub_id, TIMEOUT).await;

    pub_raw
        .send_raw(&RawPacketBuilder::publish_qos2_with_message_expiry(
            "test/expiry-sent",
            b"expiring-payload",
            10,
            1,
        ))
        .await
        .unwrap();
    pub_raw
        .expect_pubrec(TIMEOUT)
        .await
        .expect("expected PUBREC for publisher");
    pub_raw
        .send_raw(&RawPacketBuilder::pubrel(10))
        .await
        .unwrap();
    pub_raw
        .expect_pubcomp(TIMEOUT)
        .await
        .expect("expected PUBCOMP for publisher");

    let publish_pid = sub_raw
        .expect_publish_qos2(TIMEOUT)
        .await
        .expect("subscriber must receive QoS 2 PUBLISH");

    tokio::time::sleep(Duration::from_millis(1500)).await;

    sub_raw
        .send_raw(&RawPacketBuilder::pubrec(publish_pid))
        .await
        .unwrap();

    let pubrel = sub_raw.expect_pubrel_raw(TIMEOUT).await;
    assert!(
        pubrel.is_some(),
        "[MQTT-4.3.3-7] broker must send PUBREL even after message expiry elapsed — must not discard after PUBLISH sent"
    );

    let (_, pubrel_pid, _) = pubrel.unwrap();
    sub_raw
        .send_raw(&RawPacketBuilder::pubcomp(pubrel_pid))
        .await
        .unwrap();
}

/// `[MQTT-4.3.3-13]` Server MUST continue the `QoS` 2 acknowledgement sequence
/// even if it has applied message expiry.
#[conformance_test(
    ids = ["MQTT-4.3.3-13"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn qos2_continues_despite_expiry(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("exp-cont");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_qos2_with_message_expiry(
        "test/expiry-continue",
        b"expiring",
        20,
        1,
    ))
    .await
    .unwrap();

    let pubrec = raw
        .expect_pubrec(TIMEOUT)
        .await
        .expect("expected PUBREC from broker");
    assert_eq!(pubrec.0, 20, "PUBREC packet ID must match");

    tokio::time::sleep(Duration::from_millis(1500)).await;

    raw.send_raw(&RawPacketBuilder::pubrel(20)).await.unwrap();

    let pubcomp = raw.expect_pubcomp(TIMEOUT).await;
    assert!(
        pubcomp.is_some(),
        "[MQTT-4.3.3-13] broker must send PUBCOMP even after message expiry — must continue QoS 2 sequence"
    );
    assert_eq!(pubcomp.unwrap().0, 20, "PUBCOMP packet ID must match");
}
