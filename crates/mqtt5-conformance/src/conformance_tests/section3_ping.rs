//! Section 3.12 — PINGREQ/PINGRESP and Section 3.1.2-11 — keep-alive timeout.
//!
//! Proof-of-concept module for the `#[conformance_test]` proc-macro and the
//! `mqtt5-conformance-cli` runner. Exercises the raw-client path because the
//! assertions under test live at the packet level.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

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

/// Send 3 PINGREQs in sequence, verify all 3 PINGRESPs are received.
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

/// `[MQTT-3.1.2-11]` Connect with keep-alive=2s, go silent, verify broker
/// closes the connection within 1.5x the keep-alive (3s), with 1s margin.
#[conformance_test(
    ids = ["MQTT-3.1.2-11"],
    requires = ["transport.tcp"],
)]
async fn keepalive_timeout_closes_connection(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("ka-timeout");
    raw.send_raw(&RawPacketBuilder::connect_with_keepalive(&client_id, 2))
        .await
        .unwrap();
    raw.expect_connack(TIMEOUT).await.expect("expected CONNACK");

    assert!(
        raw.expect_disconnect(Duration::from_secs(5)).await,
        "[MQTT-3.1.2-11] server must close connection when keep-alive expires (2s * 1.5 = 3s)"
    );
}

/// Connect with keep-alive=0 (disabled), go silent for 5s, verify connection
/// stays open by sending a PINGREQ and getting a PINGRESP.
#[conformance_test(
    ids = ["MQTT-3.1.2-11"],
    requires = ["transport.tcp"],
)]
async fn keepalive_zero_no_timeout(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("ka-zero");
    raw.send_raw(&RawPacketBuilder::connect_with_keepalive(&client_id, 0))
        .await
        .unwrap();
    raw.expect_connack(TIMEOUT).await.expect("expected CONNACK");

    tokio::time::sleep(Duration::from_secs(5)).await;

    raw.send_raw(&RawPacketBuilder::pingreq()).await.unwrap();
    assert!(
        raw.expect_pingresp(TIMEOUT).await,
        "with keep-alive=0 the connection must remain open indefinitely"
    );
}

/// Connect with keep-alive=2s, send PINGREQ every second for 5s. The
/// PINGREQs reset the keep-alive timer so the connection must stay alive.
#[conformance_test(
    ids = ["MQTT-3.1.2-11"],
    requires = ["transport.tcp"],
)]
async fn pingreq_resets_keepalive(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("ka-reset");
    raw.send_raw(&RawPacketBuilder::connect_with_keepalive(&client_id, 2))
        .await
        .unwrap();
    raw.expect_connack(TIMEOUT).await.expect("expected CONNACK");

    for _ in 0..5 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        raw.send_raw(&RawPacketBuilder::pingreq()).await.unwrap();
        assert!(
            raw.expect_pingresp(TIMEOUT).await,
            "PINGREQ should keep connection alive and get PINGRESP"
        );
    }
}
