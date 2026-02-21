use mqtt5_conformance::harness::{unique_client_id, ConformanceBroker};
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// [MQTT-4.13.1-1] When a Server detects a Malformed Packet or Protocol Error,
/// and a Reason Code is given in the specification, it MUST close the Network
/// Connection.
///
/// Sends a packet with first byte 0x00 (packet type 0, which is reserved
/// and has no valid interpretation). The server must close the connection.
#[tokio::test]
async fn malformed_packet_closes_connection() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
///
/// Triggers a protocol error by sending a second CONNECT packet on the same
/// connection. The server should send DISCONNECT and close the connection.
#[tokio::test]
async fn protocol_error_disconnect_closes_connection() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
