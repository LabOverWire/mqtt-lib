//! Section 3.14 — DISCONNECT packet behavior.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use crate::test_client::TestClient;
use bytes::BytesMut;
use mqtt5_protocol::packet::disconnect::DisconnectPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::types::{ConnectOptions, SubscribeOptions};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// `[MQTT-3.14.4-3]` On receipt of DISCONNECT with Reason Code 0x00 the
/// Server MUST discard the Will Message without publishing it.
#[conformance_test(
    ids = ["MQTT-3.14.4-3"],
    requires = ["transport.tcp"],
)]
async fn disconnect_normal_suppresses_will(sut: SutHandle) {
    let will_id = unique_client_id("disc-normal");
    let will_topic = format!("will/{will_id}");

    let subscriber = TestClient::connect_with_prefix(&sut, "disc-norm-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&will_topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::connect_with_will_and_keepalive(
        &will_id, 60,
    ))
    .await
    .unwrap();
    raw.expect_connack(TIMEOUT).await.expect("expected CONNACK");

    raw.send_raw(&RawPacketBuilder::disconnect_normal())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        subscription.count(),
        0,
        "[MQTT-3.14.4-3] will must NOT be published on normal disconnect (0x00)"
    );

    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.4-5]` DISCONNECT with reason code 0x04
/// (`DisconnectWithWillMessage`) MUST still trigger will publication.
#[conformance_test(
    ids = ["MQTT-3.1.4-5"],
    requires = ["transport.tcp"],
)]
async fn disconnect_with_will_message_publishes_will(sut: SutHandle) {
    let will_id = unique_client_id("disc-0x04");
    let will_topic = format!("will/{will_id}");

    let subscriber = TestClient::connect_with_prefix(&sut, "disc-04-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&will_topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::connect_with_will_and_keepalive(
        &will_id, 60,
    ))
    .await
    .unwrap();
    raw.expect_connack(TIMEOUT).await.expect("expected CONNACK");

    raw.send_raw(&RawPacketBuilder::disconnect_with_reason(0x04))
        .await
        .unwrap();

    let msg = subscription
        .expect_publish(Duration::from_secs(3))
        .await
        .expect("will must be published when DISCONNECT reason is 0x04");
    assert_eq!(msg.topic, will_topic);
    assert_eq!(msg.payload, b"offline");

    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.4-5]` Connect with will + keepalive=2s, drop TCP without
/// sending DISCONNECT. Will MUST be published after keep-alive timeout.
#[conformance_test(
    ids = ["MQTT-3.1.4-5"],
    requires = ["transport.tcp"],
)]
async fn tcp_drop_publishes_will(sut: SutHandle) {
    let will_id = unique_client_id("disc-drop");
    let will_topic = format!("will/{will_id}");

    let subscriber = TestClient::connect_with_prefix(&sut, "disc-drop-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&will_topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::connect_with_will_and_keepalive(
        &will_id, 2,
    ))
    .await
    .unwrap();
    raw.expect_connack(TIMEOUT).await.expect("expected CONNACK");

    drop(raw);

    let msg = subscription
        .expect_publish(Duration::from_secs(6))
        .await
        .expect("will must be published after TCP drop");
    assert_eq!(msg.topic, will_topic);
    assert_eq!(msg.payload, b"offline");

    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.14.2-1]` Send DISCONNECT with various valid reason codes
/// (0x00, 0x04, 0x80) and verify the broker accepts them cleanly.
#[conformance_test(
    ids = ["MQTT-3.14.2-1"],
    requires = ["transport.tcp"],
)]
async fn disconnect_valid_reason_codes_accepted(sut: SutHandle) {
    for &reason in &[0x00u8, 0x04, 0x80] {
        let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
            .await
            .unwrap();
        let client_id = unique_client_id(&format!("disc-rc-{reason:02x}"));
        raw.connect_and_establish(&client_id, TIMEOUT).await;

        let packet = if reason == 0x00 {
            RawPacketBuilder::disconnect_normal()
        } else {
            RawPacketBuilder::disconnect_with_reason(reason)
        };
        raw.send_raw(&packet).await.unwrap();

        assert!(
            raw.expect_disconnect(TIMEOUT).await,
            "[MQTT-3.14.2-1] broker must accept valid reason code 0x{reason:02x} and close connection"
        );
    }
}

/// `[MQTT-3.14.2-1]` Send DISCONNECT with an invalid reason code byte
/// (0x03 is not in the valid set). The broker must close the connection.
#[conformance_test(
    ids = ["MQTT-3.14.2-1"],
    requires = ["transport.tcp"],
)]
async fn disconnect_invalid_reason_code_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("disc-bad-rc");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::disconnect_with_reason(0x03))
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.14.2-1] broker must reject invalid reason code 0x03 and close connection"
    );
}

/// `[MQTT-4.13.2-1]` Sending a second CONNECT packet is a protocol error.
/// The server MUST send DISCONNECT with an error reason code and close
/// the connection.
#[conformance_test(
    ids = ["MQTT-4.13.2-1"],
    requires = ["transport.tcp"],
)]
async fn server_disconnect_on_protocol_error(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("disc-proto-err");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::valid_connect("second-connect"))
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "server must disconnect client after receiving second CONNECT"
    );
}

/// `[MQTT-3.14.2-1]` When the server disconnects a client due to a
/// protocol error, the DISCONNECT packet MUST use a reason code from the
/// specification's allowed set.
#[conformance_test(
    ids = ["MQTT-3.14.2-1"],
    requires = ["transport.tcp"],
)]
async fn server_disconnect_uses_valid_reason_code(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("disc-rc-check");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::valid_connect("second-connect-2"))
        .await
        .unwrap();

    let valid_disconnect_codes: &[u8] = &[
        0x00, 0x04, 0x80, 0x81, 0x82, 0x83, 0x87, 0x89, 0x8B, 0x8D, 0x8E, 0x93, 0x94, 0x95, 0x96,
        0x97, 0x98, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F, 0xA1, 0xA2,
    ];

    if let Some(reason_code) = raw.expect_disconnect_packet(TIMEOUT).await {
        assert!(
            valid_disconnect_codes.contains(&reason_code),
            "[MQTT-3.14.2-1] server DISCONNECT reason code 0x{reason_code:02x} is not in the valid set"
        );
    }
}

/// `[MQTT-3.14.4-1]` / `[MQTT-3.14.4-2]` After sending DISCONNECT, the
/// sender MUST close the connection. Verify no PINGRESP arrives in
/// response to a PINGREQ sent after client DISCONNECT.
#[conformance_test(
    ids = ["MQTT-3.14.4-1", "MQTT-3.14.4-2"],
    requires = ["transport.tcp"],
)]
async fn no_packets_after_client_disconnect(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("disc-no-pkt");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::disconnect_normal())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let _ = raw.send_raw(&RawPacketBuilder::pingreq()).await;

    assert!(
        !raw.expect_pingresp(Duration::from_secs(1)).await,
        "[MQTT-3.14.4-1] no PINGRESP should be received after client sent DISCONNECT"
    );
}

fn disconnect_with_session_expiry(session_expiry: u32) -> Vec<u8> {
    let mut disconnect = DisconnectPacket::new(ReasonCode::Success);
    disconnect
        .properties
        .set_session_expiry_interval(session_expiry);
    let mut buf = BytesMut::new();
    disconnect.encode(&mut buf).expect("DISCONNECT encodes");
    buf.to_vec()
}

async fn reconnect_session_present(sut: &SutHandle, client_id: &str) -> bool {
    let opts = ConnectOptions::new(client_id)
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let client = TestClient::connect_with_options(sut, opts)
        .await
        .expect("reconnect failed");
    let present = client.session_present();
    client.disconnect().await.expect("disconnect failed");
    present
}

/// `[MQTT-4.1.0-2]` A Session Expiry Interval sent on DISCONNECT replaces the
/// one from CONNECT (§3.14.2.2.2). A value of 0 makes the session end when the
/// Network Connection closes, so the Server discards the Session State.
#[conformance_test(
    ids = ["MQTT-4.1.0-2"],
    requires = ["transport.tcp"],
)]
async fn disconnect_session_expiry_zero_discards_session(sut: SutHandle) {
    let client_id = unique_client_id("disc-sei0");

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::connect_with_session_expiry(
        &client_id, 300,
    ))
    .await
    .unwrap();
    raw.expect_connack(TIMEOUT).await.expect("expected CONNACK");
    raw.send_raw(&disconnect_with_session_expiry(0))
        .await
        .unwrap();
    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "server must close after DISCONNECT"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(
        !reconnect_session_present(&sut, &client_id).await,
        "[MQTT-4.1.0-2] Session Expiry 0 on DISCONNECT must end the session at disconnect"
    );
}

/// `[MQTT-3.1.2-23]` The Server MUST store the Session State after the
/// Network Connection closes if the Session Expiry Interval is greater than 0.
/// A non-zero value sent on DISCONNECT replaces a shorter CONNECT value.
#[conformance_test(
    ids = ["MQTT-3.1.2-23"],
    requires = ["transport.tcp"],
)]
async fn disconnect_session_expiry_extends_session(sut: SutHandle) {
    let client_id = unique_client_id("disc-seiext");

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::connect_with_session_expiry(
        &client_id, 1,
    ))
    .await
    .unwrap();
    raw.expect_connack(TIMEOUT).await.expect("expected CONNACK");
    raw.send_raw(&disconnect_with_session_expiry(300))
        .await
        .unwrap();
    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "server must close after DISCONNECT"
    );
    tokio::time::sleep(Duration::from_millis(2500)).await;

    assert!(
        reconnect_session_present(&sut, &client_id).await,
        "[MQTT-3.1.2-23] the DISCONNECT Session Expiry of 300s replaces the CONNECT value of 1s"
    );
}

/// `[MQTT-4.13.1-1]` If CONNECT set a Session Expiry Interval of 0, a
/// non-zero value on DISCONNECT is a Protocol Error (§3.14.2.2.2): the Server
/// sends DISCONNECT 0x82 and MUST close the Network Connection.
#[conformance_test(
    ids = ["MQTT-4.13.1-1"],
    requires = ["transport.tcp"],
)]
async fn disconnect_session_expiry_after_zero_is_protocol_error(sut: SutHandle) {
    let client_id = unique_client_id("disc-seierr");

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::connect_with_session_expiry(
        &client_id, 0,
    ))
    .await
    .unwrap();
    raw.expect_connack(TIMEOUT).await.expect("expected CONNACK");
    raw.send_raw(&disconnect_with_session_expiry(300))
        .await
        .unwrap();

    assert_eq!(
        raw.expect_disconnect_packet(TIMEOUT).await,
        Some(0x82),
        "[MQTT-4.13.1-1] non-zero Session Expiry on DISCONNECT after 0 on CONNECT is a Protocol Error"
    );
    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-4.13.1-1] Server must close the connection after the Protocol Error"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !reconnect_session_present(&sut, &client_id).await,
        "the session keeps its Session Expiry of 0 and ends at disconnect"
    );
}
