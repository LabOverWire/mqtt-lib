//! Section 3.12 — PINGREQ/PINGRESP and Section 3.1.2-11 — keep-alive timeout.
//!
//! Proof-of-concept module for the `#[conformance_test]` proc-macro and the
//! `mqtt5-conformance-cli` runner. Exercises the raw-client path because the
//! assertions under test live at the packet level.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use mqtt5_protocol::protocol::v5::properties::PropertyId;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// `[MQTT-3.12.4-1]` Server MUST send PINGRESP in response to PINGREQ.
#[conformance_test(
    ids = ["MQTT-3.12.4-1"],
    requires = ["transport.tcp"],
)]
async fn pingresp_sent_on_pingreq(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("ping-resp");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::pingreq()).await.unwrap();

    assert!(
        raw.expect_pingresp(TIMEOUT).await,
        "[MQTT-3.12.4-1] server must send PINGRESP in response to PINGREQ"
    );
}

/// [MQTT-3.12.4-1] The Server MUST answer each of three PINGREQs in a row with
/// a PINGRESP.
#[conformance_test(
    ids = ["MQTT-3.12.4-1"],
    requires = ["transport.tcp"],
)]
async fn multiple_pingreqs_all_responded(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("ping-multi");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    for i in 0..3 {
        raw.send_raw(&RawPacketBuilder::pingreq()).await.unwrap();
        assert!(
            raw.expect_pingresp(TIMEOUT).await,
            "[MQTT-3.12.4-1] server must respond to PINGREQ #{}",
            i + 1
        );
    }
}

/// `[MQTT-3.1.2-22]` `[MQTT-3.2.2-22]` With Keep Alive 6 s and no Server Keep
/// Alive, the Server must close the connection after 1.5 times the Keep Alive
/// (9 s) of silence. The 2 s margin allows a broker that checks once a second,
/// and a close at twice the Keep Alive (12 s) still fails.
#[conformance_test(
    ids = ["MQTT-3.1.2-22", "MQTT-3.2.2-22"],
    requires = ["transport.tcp", "no_server_keep_alive"],
)]
async fn keepalive_timeout_closes_connection(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("ka-timeout");
    raw.send_raw(&RawPacketBuilder::connect_with_keepalive(&client_id, 6))
        .await
        .unwrap();
    expect_connack_without_server_keep_alive(&mut raw).await;

    assert!(
        raw.expect_disconnect(Duration::from_secs(11)).await,
        "[MQTT-3.1.2-22] server must close the connection after 9 s of silence with Keep Alive 6 s"
    );
}

/// [MQTT-3.2.2-22] With Keep Alive 0 and no Server Keep Alive, the keep alive
/// is off, so the connection must stay open after 5 s of silence.
#[conformance_test(
    ids = ["MQTT-3.2.2-22"],
    requires = ["transport.tcp", "no_server_keep_alive"],
)]
async fn keepalive_zero_no_timeout(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("ka-zero");
    raw.send_raw(&RawPacketBuilder::connect_with_keepalive(&client_id, 0))
        .await
        .unwrap();
    expect_connack_without_server_keep_alive(&mut raw).await;

    tokio::time::sleep(Duration::from_secs(5)).await;

    raw.send_raw(&RawPacketBuilder::pingreq()).await.unwrap();
    assert!(
        raw.expect_pingresp(TIMEOUT).await,
        "[MQTT-3.2.2-22] with keep-alive=0 the connection must remain open indefinitely"
    );
}

/// [MQTT-3.12.4-1] With Keep Alive 2 s and no Server Keep Alive, a PINGREQ
/// every second for 5 s must get a PINGRESP each time.
#[conformance_test(
    ids = ["MQTT-3.12.4-1"],
    requires = ["transport.tcp", "no_server_keep_alive"],
)]
async fn pingreq_resets_keepalive(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("ka-reset");
    raw.send_raw(&RawPacketBuilder::connect_with_keepalive(&client_id, 2))
        .await
        .unwrap();
    expect_connack_without_server_keep_alive(&mut raw).await;

    for _ in 0..5 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        raw.send_raw(&RawPacketBuilder::pingreq()).await.unwrap();
        assert!(
            raw.expect_pingresp(TIMEOUT).await,
            "PINGREQ should keep connection alive and get PINGRESP"
        );
    }
}

async fn expect_connack_without_server_keep_alive(raw: &mut RawMqttClient) {
    let connack = raw
        .expect_connack_packet(TIMEOUT)
        .await
        .expect("expected CONNACK");
    let server_keep_alive = connack.properties.get(PropertyId::ServerKeepAlive);
    assert!(
        server_keep_alive.is_none(),
        "the CONNACK carries Server Keep Alive {server_keep_alive:?}, but the SUT profile declares server_keep_alive = 0"
    );
}
