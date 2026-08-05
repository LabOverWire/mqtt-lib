//! Section 3.2 — CONNACK Maximum Packet Size enforcement.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::packet_parser::ParsedConnAck;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// `[MQTT-3.2.2-19]` The Server MUST NOT send a Reason String on CONNACK if it
/// would increase the packet beyond the Maximum Packet Size the Client
/// specified. A CONNECT carrying an Authentication Method against a broker that
/// does not support enhanced authentication yields a Bad Authentication Method
/// CONNACK whose Reason String alone is larger than the tiny stated limit, so
/// the property must be dropped while the packet is still sent.
#[conformance_test(
    ids = ["MQTT-3.2.2-19"],
    requires = ["transport.tcp"],
)]
async fn connack_reason_string_omitted_over_max_packet_size(sut: SutHandle) {
    let mut client = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("mps");
    client
        .send_raw(
            &RawPacketBuilder::connect_with_auth_method_and_max_packet_size(
                &client_id,
                "SCRAM-SHA-1",
                30,
            ),
        )
        .await
        .unwrap();

    let data = client
        .read_packet_bytes(TIMEOUT)
        .await
        .expect("broker must send a CONNACK");
    let connack = ParsedConnAck::parse(&data).expect("delivered packet must be a CONNACK");
    assert_eq!(
        connack.reason_code, 0x8C,
        "expected Bad Authentication Method (0x8C), got {:#04x}",
        connack.reason_code
    );
    assert!(
        connack.properties.reason_string.is_none(),
        "[MQTT-3.2.2-19] CONNACK Reason String must be omitted when it would exceed the client's \
         Maximum Packet Size (stated 30), found {:?}",
        connack.properties.reason_string
    );
    assert!(
        data.len() <= 30,
        "[MQTT-3.2.2-19] CONNACK is {} bytes, exceeding the client's Maximum Packet Size (30)",
        data.len()
    );
}

/// `[MQTT-3.4.2-2]` The Server MUST NOT send a Reason String on PUBACK if it
/// would increase the packet beyond the Maximum Packet Size the Client
/// specified. A broker with `maximum_qos = 0` rejects a `QoS` 1 PUBLISH with a
/// PUBACK whose Reason String pushes it past a small stated limit, so the
/// property must be dropped while the PUBACK is still sent.
#[cfg(all(test, feature = "inprocess-fixture"))]
#[tokio::test]
async fn puback_reason_string_omitted_over_max_packet_size() {
    let config = mqtt5::broker::config::BrokerConfig::default().with_maximum_qos(0);
    let sut = crate::sut::inprocess_sut_with_config(config).await;

    let mut client = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("mpsp");
    client
        .send_raw(&RawPacketBuilder::connect_with_max_packet_size(
            &client_id, 35,
        ))
        .await
        .unwrap();
    client
        .expect_connack(TIMEOUT)
        .await
        .expect("CONNACK must fit the 35-byte limit and be delivered");

    let topic = format!("mps/{}", unique_client_id("t"));
    client
        .send_raw(&RawPacketBuilder::publish_qos1(&topic, b"x", 1))
        .await
        .unwrap();

    let data = client
        .read_packet_bytes(TIMEOUT)
        .await
        .expect("broker must send a PUBACK");
    assert!(
        data.len() >= 5,
        "PUBACK truncated, cannot read reason code: {data:02x?}"
    );
    assert_eq!(
        data[0] & 0xF0,
        0x40,
        "expected a PUBACK, got {:#04x}",
        data[0]
    );
    assert_eq!(
        data[4], 0x9B,
        "expected QoS Not Supported (0x9B) reason code, got {:#04x}",
        data[4]
    );
    assert!(
        data.len() <= 35,
        "[MQTT-3.4.2-2] PUBACK is {} bytes, exceeding the client's Maximum Packet Size (35) — the \
         Reason String was not omitted",
        data.len()
    );
}

/// MQTT v5.0 3.1.2.11.4: a Maximum Packet Size of 0 is a Protocol Error. The
/// broker must reject the connection rather than accept a zero limit, which
/// would otherwise make every outbound packet — including the CONNACK itself —
/// exceed the limit and be discarded.
#[cfg(all(test, feature = "inprocess-fixture"))]
#[tokio::test]
async fn zero_max_packet_size_rejected() {
    let sut = crate::sut::inprocess_sut().await;
    let mut client = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    client
        .send_raw(&RawPacketBuilder::connect_with_max_packet_size(
            "zero-mps", 0,
        ))
        .await
        .unwrap();

    let data = client
        .read_packet_bytes(TIMEOUT)
        .await
        .expect("broker must send a CONNACK rejecting the zero Maximum Packet Size");
    let connack = ParsedConnAck::parse(&data).expect("delivered packet must be a CONNACK");
    assert_eq!(
        connack.reason_code, 0x82,
        "expected Protocol Error (0x82) for a Maximum Packet Size of 0, got {:#04x}",
        connack.reason_code
    );
}
