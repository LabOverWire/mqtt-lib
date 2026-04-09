//! Section 4.13 — Error Handling.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// [MQTT-4.13.1-1] When a Server detects a Malformed Packet or Protocol Error,
/// and a Reason Code is given in the specification, it MUST close the Network
/// Connection.
#[conformance_test(
    ids = ["MQTT-4.13.1-1"],
    requires = ["transport.tcp"],
)]
async fn malformed_packet_closes_connection(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("malformed");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    let garbage_packet: &[u8] = &[0x00, 0x00];
    raw.send_raw(garbage_packet).await.unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-4.13.1-1] server must close connection on malformed/invalid packet type"
    );
}

/// [MQTT-4.13.2-1] The Server MUST perform one of the following if it detects
/// a Protocol Error: send a DISCONNECT with an appropriate Reason Code, and
/// then close the Network Connection.
#[conformance_test(
    ids = ["MQTT-4.13.2-1"],
    requires = ["transport.tcp"],
)]
async fn protocol_error_disconnect_closes_connection(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("proto-err");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::valid_connect("second-connect"))
        .await
        .unwrap();

    let disconnect_reason = raw.expect_disconnect_packet(TIMEOUT).await;
    match disconnect_reason {
        Some(reason) => {
            assert!(
                reason >= 0x80,
                "[MQTT-4.13.2-1] DISCONNECT reason code must indicate error (>= 0x80), got {reason:#04x}"
            );
            assert!(
                raw.expect_disconnect(TIMEOUT).await,
                "[MQTT-4.13.2-1] connection must be closed after DISCONNECT with error reason"
            );
        }
        None => {
            assert!(
                raw.expect_disconnect(Duration::from_millis(100)).await,
                "[MQTT-4.13.2-1] server must close connection on protocol error (second CONNECT)"
            );
        }
    }
}
