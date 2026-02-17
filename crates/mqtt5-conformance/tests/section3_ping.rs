#![allow(clippy::cast_possible_truncation)]

mod common;

use common::{unique_client_id, ConformanceBroker, RawMqttClient, RawPacketBuilder};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Group 1: PINGREQ/PINGRESP Exchange — Section 3.12
// ---------------------------------------------------------------------------

/// `[MQTT-3.12.4-1]` Server MUST send PINGRESP in response to PINGREQ.
#[tokio::test]
async fn pingresp_sent_on_pingreq() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
#[tokio::test]
async fn multiple_pingreqs_all_responded() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

// ---------------------------------------------------------------------------
// Group 2: Keep-Alive Timeout Enforcement — Section 3.1.2-11
// ---------------------------------------------------------------------------

/// `[MQTT-3.1.2-11]` Connect with keep-alive=2s, go silent, verify broker
/// closes the connection within 1.5x the keep-alive (3s), with 1s margin.
#[tokio::test]
async fn keepalive_timeout_closes_connection() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
#[tokio::test]
async fn keepalive_zero_no_timeout() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
#[tokio::test]
async fn pingreq_resets_keepalive() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
