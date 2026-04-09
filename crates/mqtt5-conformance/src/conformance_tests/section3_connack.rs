//! Section 3.2 — CONNACK packet structure and behaviour.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{put_mqtt_string, wrap_fixed_header, RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use mqtt5_protocol::protocol::v5::properties::PropertyId;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// `[MQTT-3.2.2-1]` Byte 1 of CONNACK is the Connect Acknowledge Flags.
/// Bits 7-1 are reserved and MUST be set to 0.
#[conformance_test(
    ids = ["MQTT-3.2.2-1"],
    requires = ["transport.tcp"],
)]
async fn connack_reserved_flags_zero(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("flags");
    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    let connack = raw.expect_connack(TIMEOUT).await;
    assert!(connack.is_some(), "Must receive CONNACK");
    let (flags, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "Reason code must be Success");
    assert_eq!(
        flags & 0xFE,
        0,
        "[MQTT-3.2.2-1] CONNACK flags bits 7-1 must be zero"
    );
}

/// `[MQTT-3.2.0-2]` The Server MUST NOT send more than one CONNACK in a
/// Network Connection.
#[conformance_test(
    ids = ["MQTT-3.2.0-2"],
    requires = ["transport.tcp"],
)]
async fn connack_only_one_per_connection(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("one-connack");
    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    let connack = raw.expect_connack(TIMEOUT).await;
    assert!(connack.is_some(), "Must receive first CONNACK");
    let (_, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "Must accept connection");

    raw.send_raw(&RawPacketBuilder::subscribe("test/connack", 0))
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::publish_qos0("test/connack", b"hello"))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut connack_count = 0u32;
    loop {
        let data = raw.read_packet_bytes(Duration::from_millis(200)).await;
        match data {
            Some(bytes) if !bytes.is_empty() => {
                if bytes[0] == 0x20 {
                    connack_count += 1;
                }
            }
            _ => break,
        }
    }

    assert_eq!(
        connack_count, 0,
        "[MQTT-3.2.0-2] Server must not send a second CONNACK"
    );
}

/// `[MQTT-3.2.2-6]` If a Server sends a CONNACK packet containing a
/// non-zero Reason Code it MUST set Session Present to 0.
#[conformance_test(
    ids = ["MQTT-3.2.2-6"],
    requires = ["transport.tcp"],
)]
async fn connack_session_present_zero_on_error(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_protocol_version(99))
        .await
        .unwrap();

    let connack = raw.expect_connack(TIMEOUT).await;
    assert!(connack.is_some(), "Must receive error CONNACK");
    let (flags, reason) = connack.unwrap();
    assert_ne!(reason, 0x00, "Must be a non-success reason code");
    assert_eq!(
        flags & 0x01,
        0,
        "[MQTT-3.2.2-6] Session Present must be 0 when reason code is non-zero"
    );
}

/// `[MQTT-3.2.2-7]` If a Server sends a CONNACK packet containing a Reason
/// Code of 128 or greater it MUST then close the Network Connection.
#[conformance_test(
    ids = ["MQTT-3.2.2-7"],
    requires = ["transport.tcp"],
)]
async fn connack_error_code_closes_connection(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_protocol_version(99))
        .await
        .unwrap();

    let response = raw.read_packet_bytes(TIMEOUT).await;
    assert!(response.is_some(), "Must receive CONNACK");
    let data = response.unwrap();
    assert_eq!(data[0], 0x20, "Must be CONNACK");

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.2.2-7] Server must close connection after error CONNACK (reason >= 128)"
    );
}

/// `[MQTT-3.2.2-8]` The Server sending the CONNACK packet MUST use one of
/// the Connect Reason Code values.
#[conformance_test(
    ids = ["MQTT-3.2.2-8"],
    requires = ["transport.tcp"],
)]
async fn connack_uses_valid_reason_code(sut: SutHandle) {
    let mut raw_ok = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("valid-rc");
    raw_ok
        .send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();
    let ok_connack = raw_ok.expect_connack_packet(TIMEOUT).await;
    assert!(ok_connack.is_some(), "Must receive success CONNACK");
    assert_eq!(
        ok_connack.unwrap().reason_code,
        ReasonCode::Success,
        "[MQTT-3.2.2-8] Success CONNACK must have reason code 0x00"
    );

    let mut raw_err = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw_err
        .send_raw(&RawPacketBuilder::connect_with_protocol_version(99))
        .await
        .unwrap();
    let err_connack = raw_err.expect_connack_packet(TIMEOUT).await;
    assert!(err_connack.is_some(), "Must receive error CONNACK");
    assert_eq!(
        err_connack.unwrap().reason_code,
        ReasonCode::UnsupportedProtocolVersion,
        "[MQTT-3.2.2-8] Unsupported protocol version must use reason code 0x84"
    );
}

/// `[MQTT-3.2.2-16]` When two clients connect with empty client IDs, the
/// server must assign different unique `ClientID`s.
#[conformance_test(
    ids = ["MQTT-3.2.2-16"],
    requires = ["transport.tcp"],
)]
async fn connack_assigned_client_id_unique(sut: SutHandle) {
    let mut ids = Vec::new();
    for _ in 0..2 {
        let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
            .await
            .unwrap();
        raw.send_raw(&build_connect_empty_client_id())
            .await
            .unwrap();

        let connack = raw
            .expect_connack_packet(TIMEOUT)
            .await
            .expect("Must receive CONNACK");

        assert_eq!(connack.reason_code, ReasonCode::Success);
        let assigned = connack.properties.get(PropertyId::AssignedClientIdentifier);
        assert!(
            assigned.is_some(),
            "[MQTT-3.2.2-16] CONNACK must include Assigned Client Identifier"
        );
        if let Some(mqtt5_protocol::PropertyValue::Utf8String(id)) = assigned {
            ids.push(id.clone());
        } else {
            panic!("Assigned Client Identifier has wrong type");
        }
    }

    assert_ne!(
        ids[0], ids[1],
        "[MQTT-3.2.2-16] Assigned Client Identifiers must be unique"
    );
}

fn build_connect_empty_client_id() -> Vec<u8> {
    use bytes::{BufMut, BytesMut};

    let mut body = BytesMut::new();
    body.put_u16(4);
    body.put_slice(b"MQTT");
    body.put_u8(5);
    body.put_u8(0x02);
    body.put_u16(60);
    body.put_u8(0);
    put_mqtt_string(&mut body, "");

    wrap_fixed_header(0x10, &body)
}
