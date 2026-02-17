#![allow(clippy::cast_possible_truncation)]

mod common;

use common::{
    connected_client, unique_client_id, ConformanceBroker, MessageCollector, RawMqttClient,
    RawPacketBuilder,
};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Group 1: Will Suppression/Publication on Disconnect
// ---------------------------------------------------------------------------

/// `[MQTT-3.14.4-3]` On receipt of DISCONNECT with Reason Code 0x00 the
/// Server MUST discard the Will Message without publishing it.
#[tokio::test]
async fn disconnect_normal_suppresses_will() {
    let broker = ConformanceBroker::start().await;
    let will_id = unique_client_id("disc-normal");
    let will_topic = format!("will/{will_id}");

    let collector = MessageCollector::new();
    let subscriber = connected_client("disc-norm-sub", &broker).await;
    subscriber
        .subscribe(&will_topic, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
        collector.count().await,
        0,
        "[MQTT-3.14.4-3] will must NOT be published on normal disconnect (0x00)"
    );

    subscriber.disconnect().await.expect("disconnect failed");
}

/// DISCONNECT with reason code 0x04 (`DisconnectWithWillMessage`) MUST
/// still trigger will publication.
#[tokio::test]
async fn disconnect_with_will_message_publishes_will() {
    let broker = ConformanceBroker::start().await;
    let will_id = unique_client_id("disc-0x04");
    let will_topic = format!("will/{will_id}");

    let collector = MessageCollector::new();
    let subscriber = connected_client("disc-04-sub", &broker).await;
    subscriber
        .subscribe(&will_topic, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

    assert!(
        collector.wait_for_messages(1, Duration::from_secs(3)).await,
        "will must be published when DISCONNECT reason is 0x04 (DisconnectWithWillMessage)"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].topic, will_topic);
    assert_eq!(msgs[0].payload, b"offline");

    subscriber.disconnect().await.expect("disconnect failed");
}

/// Connect with will + keepalive=2s, drop TCP without sending DISCONNECT.
/// Will MUST be published after keep-alive timeout.
#[tokio::test]
async fn tcp_drop_publishes_will() {
    let broker = ConformanceBroker::start().await;
    let will_id = unique_client_id("disc-drop");
    let will_topic = format!("will/{will_id}");

    let collector = MessageCollector::new();
    let subscriber = connected_client("disc-drop-sub", &broker).await;
    subscriber
        .subscribe(&will_topic, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::connect_with_will_and_keepalive(
        &will_id, 2,
    ))
    .await
    .unwrap();
    raw.expect_connack(TIMEOUT).await.expect("expected CONNACK");

    drop(raw);

    assert!(
        collector.wait_for_messages(1, Duration::from_secs(6)).await,
        "will must be published after TCP drop (keep-alive timeout triggers abnormal disconnect)"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].topic, will_topic);
    assert_eq!(msgs[0].payload, b"offline");

    subscriber.disconnect().await.expect("disconnect failed");
}

// ---------------------------------------------------------------------------
// Group 2: Reason Code Handling
// ---------------------------------------------------------------------------

/// `[MQTT-3.14.2-1]` Send DISCONNECT with various valid reason codes
/// (0x00, 0x04, 0x80) and verify the broker accepts them cleanly.
#[tokio::test]
async fn disconnect_valid_reason_codes_accepted() {
    let broker = ConformanceBroker::start().await;

    for &reason in &[0x00u8, 0x04, 0x80] {
        let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

/// Send DISCONNECT with an invalid reason code byte (0x03 is not in the
/// valid set). The broker must close the connection.
#[tokio::test]
async fn disconnect_invalid_reason_code_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

// ---------------------------------------------------------------------------
// Group 3: Server-Initiated Disconnect
// ---------------------------------------------------------------------------

/// Sending a second CONNECT packet is a protocol error. The server MUST
/// send DISCONNECT with 0x82 (Protocol Error) and close the connection.
#[tokio::test]
async fn server_disconnect_on_protocol_error() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

/// Same as above — verify the server DISCONNECT uses a valid reason code
/// from the specification's allowed set.
#[tokio::test]
async fn server_disconnect_uses_valid_reason_code() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
            "server DISCONNECT reason code 0x{reason_code:02x} is not in the valid set"
        );
    }
}

// ---------------------------------------------------------------------------
// Group 4: Post-Disconnect Behavior
// ---------------------------------------------------------------------------

/// `[MQTT-3.14.4-1]`/`[MQTT-3.14.4-2]` After sending DISCONNECT, sender
/// must close the connection. Verify no PINGRESP to a PINGREQ sent after
/// client DISCONNECT (connection should be closed).
#[tokio::test]
async fn no_packets_after_client_disconnect() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
