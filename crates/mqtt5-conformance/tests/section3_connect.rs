//! MQTT v5.0 Section 3.1 — CONNECT packet conformance tests.
//!
//! Each test verifies a specific normative statement from the OASIS MQTT v5.0
//! specification. Tests use either the high-level [`MqttClient`] API for
//! valid-path scenarios, or [`RawMqttClient`] for sending malformed packets
//! that the normal API would reject.

#![allow(clippy::cast_possible_truncation)]

mod common;

use common::{
    connected_client, unique_client_id, ConformanceBroker, MessageCollector, RawMqttClient,
    RawPacketBuilder,
};
use mqtt5::{ConnectOptions, MqttClient, WillMessage};
use std::time::Duration;

/// `[MQTT-3.1.0-1]` After a Network Connection is established by a Client to
/// a Server, the first packet sent from the Client to the Server MUST be a
/// CONNECT packet.
///
/// Sends a PUBLISH as the first packet and verifies the server closes the
/// connection.
#[tokio::test]
async fn connect_first_packet_must_be_connect() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::publish_without_connect())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(3)).await,
        "[MQTT-3.1.0-1] Server must disconnect client that sends non-CONNECT first"
    );
}

/// `[MQTT-3.1.0-2]` The Server MUST process a second CONNECT packet sent from
/// a Client as a Protocol Error and close the Network Connection.
///
/// Sends two CONNECT packets on the same connection and verifies the server
/// disconnects after the second.
#[tokio::test]
async fn connect_second_connect_is_protocol_error() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("double");
    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    let connack = raw.expect_connack(Duration::from_secs(2)).await;
    assert!(connack.is_some(), "Should receive CONNACK");

    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(2)).await,
        "[MQTT-3.1.0-2] Server must close connection on second CONNECT"
    );
}

/// `[MQTT-3.1.2-1]` The protocol name MUST be the UTF-8 String "MQTT".
/// A Server which does not receive this MUST close the Network Connection.
///
/// Sends a CONNECT with protocol name "XXXX" and verifies disconnection.
#[tokio::test]
async fn connect_protocol_name_must_be_mqtt() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_invalid_protocol_name())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(2)).await,
        "[MQTT-3.1.2-1] Server must reject invalid protocol name"
    );
}

/// `[MQTT-3.1.2-2]` The Server MUST respond to a CONNECT packet with a CONNACK
/// with 0x84 (Unsupported Protocol Version) if the Protocol Version is not
/// supported.
///
/// Sends a CONNECT with protocol version 99 and verifies the CONNACK contains
/// reason code `0x84`.
#[tokio::test]
async fn connect_unsupported_protocol_version() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_protocol_version(99))
        .await
        .unwrap();

    let response = raw.read_packet_bytes(Duration::from_secs(2)).await;
    assert!(
        response.is_some(),
        "[MQTT-3.1.2-2] Server should send CONNACK before closing"
    );
    let data = response.unwrap();
    assert_eq!(
        data[0], 0x20,
        "[MQTT-3.1.2-2] Response must be CONNACK packet"
    );
    let reason_idx = find_connack_reason_code_index(&data);
    assert_eq!(
        data[reason_idx], 0x84,
        "[MQTT-3.1.2-2] Reason code must be 0x84 (Unsupported Protocol Version)"
    );
}

/// `[MQTT-3.1.2-3]` The Server MUST validate that the reserved flag in the
/// CONNECT packet is set to 0. If it is not 0, treat it as a Malformed Packet.
///
/// Sends a CONNECT with reserved flag (bit 0) set and verifies disconnection.
#[tokio::test]
async fn connect_reserved_flag_must_be_zero() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_reserved_flag_set())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(2)).await,
        "[MQTT-3.1.2-3] Server must close connection when reserved flag is set"
    );
}

/// `[MQTT-3.1.2-4]` If `CleanStart` is set to 1, the Client and Server MUST
/// discard any existing Session and start a new Session.
///
/// Connects twice with `clean_start=true` and verifies `session_present=false`
/// both times, confirming the first session was discarded.
#[tokio::test]
async fn connect_clean_start_new_session() {
    let broker = ConformanceBroker::start().await;
    let client_id = unique_client_id("clean");

    let opts = ConnectOptions::new(&client_id)
        .with_clean_start(true)
        .with_session_expiry_interval(300);
    let client = MqttClient::with_options(opts.clone());
    let result = Box::pin(client.connect_with_options(broker.address(), opts))
        .await
        .expect("[MQTT-3.1.2-4] Clean start connection must succeed");
    assert!(
        !result.session_present,
        "[MQTT-3.1.2-4] First connection with clean_start=true must have session_present=false"
    );
    client
        .subscribe("test/topic", |_| {})
        .await
        .expect("subscribe failed");
    client.disconnect().await.expect("disconnect failed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let opts2 = ConnectOptions::new(&client_id)
        .with_clean_start(true)
        .with_session_expiry_interval(300);
    let client2 = MqttClient::with_options(opts2.clone());
    let result2 = Box::pin(client2.connect_with_options(broker.address(), opts2))
        .await
        .expect("reconnect failed");
    assert!(
        !result2.session_present,
        "[MQTT-3.1.2-4] CleanStart=1 must discard existing session (session_present=false)"
    );
    client2.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.2-5]` If `CleanStart` is set to 0 and there is a Session
/// associated with the Client Identifier, the Server MUST resume
/// communications based on state from the existing Session.
///
/// Creates a session with `clean_start=true`, disconnects, then reconnects
/// with `clean_start=false` and verifies `session_present=true`.
#[tokio::test]
async fn connect_clean_start_false_resumes_session() {
    let broker = ConformanceBroker::start().await;
    let client_id = unique_client_id("resume");

    let opts = ConnectOptions::new(&client_id)
        .with_clean_start(true)
        .with_session_expiry_interval(300);
    let client = MqttClient::with_options(opts.clone());
    Box::pin(client.connect_with_options(broker.address(), opts))
        .await
        .expect("first connect failed");
    client
        .subscribe("test/resume", |_| {})
        .await
        .expect("subscribe failed");
    client.disconnect().await.expect("disconnect failed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let opts2 = ConnectOptions::new(&client_id)
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let client2 = MqttClient::with_options(opts2.clone());
    let result = Box::pin(client2.connect_with_options(broker.address(), opts2))
        .await
        .expect("reconnect failed");
    assert!(
        result.session_present,
        "[MQTT-3.1.2-5] CleanStart=0 with existing session must have session_present=true"
    );
    client2.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.2-6]` If the Will Flag is set to 1, the Will Properties, Will
/// Topic and Will Payload fields MUST be present in the Payload.
///
/// Connects with a valid Will Message and verifies the connection succeeds.
#[tokio::test]
async fn connect_will_flag_with_will_topic_payload() {
    let broker = ConformanceBroker::start().await;
    let will = WillMessage::new("will/topic", b"will-payload".to_vec());
    let opts = ConnectOptions::new(unique_client_id("will"))
        .with_clean_start(true)
        .with_will(will);
    let client = MqttClient::with_options(opts.clone());
    Box::pin(client.connect_with_options(broker.address(), opts))
        .await
        .expect("[MQTT-3.1.2-6] Connection with valid will must succeed");
    assert!(client.is_connected().await);
    client.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.2-7]` If the Will Flag is set to 0, then the Will `QoS` MUST be
/// set to 0.
///
/// Sends a raw CONNECT with connect flags `0x0A` (Will QoS=1 but Will Flag=0)
/// and verifies the server disconnects.
#[tokio::test]
async fn connect_will_qos_zero_without_will_flag() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let packet = build_connect_with_flags(0x0A);
    raw.send_raw(&packet).await.unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(2)).await,
        "[MQTT-3.1.2-7] Server must reject Will QoS!=0 when Will Flag=0"
    );
}

/// `[MQTT-3.1.2-8]` If the Will Flag is set to 0, then Will Retain MUST be
/// set to 0.
///
/// Sends a raw CONNECT with connect flags `0x22` (Will Retain=1 but Will
/// Flag=0) and verifies the server disconnects.
#[tokio::test]
async fn connect_will_retain_zero_without_will_flag() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let packet = build_connect_with_flags(0x22);
    raw.send_raw(&packet).await.unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(2)).await,
        "[MQTT-3.1.2-8] Server must reject Will Retain=1 when Will Flag=0"
    );
}

/// `[MQTT-3.1.2-9]` If the Will Flag is set to 1, the value of Will `QoS` can
/// be 0, 1, or 2. A value of 3 is a Malformed Packet.
///
/// Sends a raw CONNECT with connect flags `0x1E` (Will Flag=1, Will QoS=3)
/// and verifies the server disconnects.
#[tokio::test]
async fn connect_will_qos_3_is_malformed() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let packet = build_connect_with_flags(0x1E);
    raw.send_raw(&packet).await.unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(2)).await,
        "[MQTT-3.1.2-9] Server must reject Will QoS=3 as malformed"
    );
}

/// `[MQTT-3.1.2-12]` If the CONNECT packet has a fixed header flags field
/// that is not 0x00, the Server MUST treat it as a Malformed Packet.
///
/// Sends a CONNECT with fixed header byte `0x11` (flags=1) instead of `0x10`
/// (flags=0) and verifies the server disconnects.
#[tokio::test]
async fn connect_invalid_fixed_header_flags() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_invalid_fixed_header_flags())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(2)).await,
        "[MQTT-3.1.2-12] Server must reject CONNECT with non-zero fixed header flags"
    );
}

/// `[MQTT-3.1.3-3]` The Server MUST allow `ClientID`s which are between 1 and 23
/// UTF-8 encoded bytes in length, and that contain only the characters
/// `0-9`, `a-z`, `A-Z`.
///
/// Connects with a 12-character alphanumeric client ID and verifies success.
#[tokio::test]
async fn connect_valid_client_id_accepted() {
    let broker = ConformanceBroker::start().await;
    let client = MqttClient::new("abcABC012345");
    client
        .connect(broker.address())
        .await
        .expect("[MQTT-3.1.3-3] Server must accept valid client ID");
    assert!(client.is_connected().await);
    client.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.3-4]` A Server MAY allow a Client to supply a `ClientID` that has
/// a length of zero bytes, however if it does so the Server MUST treat this as
/// a special case and assign a unique `ClientID` to that Client.
///
/// Sends a raw CONNECT with an empty client ID and `clean_start=true`, then
/// verifies the CONNACK contains an Assigned Client Identifier property
/// (`0x12`).
#[tokio::test]
async fn connect_empty_client_id_server_assigns() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let connect = build_connect_empty_client_id();
    raw.send_raw(&connect).await.unwrap();

    let response = raw.read_packet_bytes(Duration::from_secs(2)).await;
    assert!(
        response.is_some(),
        "[MQTT-3.1.3-4] Server must send CONNACK"
    );
    let data = response.unwrap();
    assert_eq!(data[0], 0x20, "Response must be CONNACK");
    let reason_idx = find_connack_reason_code_index(&data);
    assert_eq!(
        data[reason_idx], 0x00,
        "[MQTT-3.1.3-4] Empty client ID with clean_start must be accepted (reason=Success)"
    );
    assert!(
        data[reason_idx + 1..].contains(&0x12),
        "[MQTT-3.1.3-4] CONNACK must contain Assigned Client Identifier property (0x12)"
    );
}

/// `[MQTT-3.1.4-1]` The Server MUST validate that the CONNECT packet matches
/// the format described in the specification and close the Network Connection
/// if it does not.
///
/// Sends a truncated CONNECT packet (header claims 50 bytes, only 3 sent)
/// and verifies the server eventually closes the connection.
#[tokio::test]
async fn connect_malformed_packet_closes_connection() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_malformed_truncated())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(12)).await,
        "[MQTT-3.1.4-1] Server must close connection on malformed CONNECT"
    );
}

/// `[MQTT-3.1.4-2]` The Server MAY check the CONNECT Packet contents are
/// consistent and if any check fails SHOULD send the CONNACK packet with a
/// non-zero return code.
///
/// Sends a CONNECT with a duplicate Session Expiry Interval property and
/// verifies the server disconnects.
#[tokio::test]
async fn connect_duplicate_property_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_duplicate_property())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(2)).await,
        "[MQTT-3.1.4-2] Server must reject CONNECT with duplicate properties"
    );
}

/// `[MQTT-3.1.4-3]` If the Server accepts a connection with `CleanStart` set to
/// 1, the Server MUST set Session Present to 0 in the CONNACK packet.
///
/// Sends a raw CONNECT with `clean_start=true` and verifies the CONNACK has
/// session present flag = 0 and reason code = Success.
#[tokio::test]
async fn connect_clean_start_session_present_zero() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("sp0");
    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    let connack = raw.expect_connack(Duration::from_secs(2)).await;
    assert!(connack.is_some(), "Must receive CONNACK");
    let (flags, reason) = connack.unwrap();
    assert_eq!(
        reason, 0x00,
        "[MQTT-3.1.4-3] Reason code must be 0x00 (Success)"
    );
    assert_eq!(
        flags & 0x01,
        0,
        "[MQTT-3.1.4-3] Session present flag must be 0 for clean start"
    );
}

/// `[MQTT-3.1.4-4]` If the Server accepts a connection with `CleanStart` set to
/// 0 and the Server has Session State for the `ClientID`, it MUST set Session
/// Present to 1 in the CONNACK packet.
///
/// Creates a session via the high-level client, disconnects, then reconnects
/// via raw client with `clean_start=false` and verifies `session_present=1`
/// in the CONNACK.
#[tokio::test]
async fn connect_clean_start_false_session_present_one() {
    let broker = ConformanceBroker::start().await;
    let client_id = unique_client_id("sp1");

    let opts = ConnectOptions::new(&client_id)
        .with_clean_start(true)
        .with_session_expiry_interval(300);
    let client = MqttClient::with_options(opts.clone());
    Box::pin(client.connect_with_options(broker.address(), opts))
        .await
        .expect("first connect failed");
    client
        .subscribe("test/sp1", |_| {})
        .await
        .expect("subscribe failed");
    client.disconnect().await.expect("disconnect failed");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    raw.send_raw(&build_connect_resume(&client_id, 300))
        .await
        .unwrap();

    let connack = raw.expect_connack(Duration::from_secs(2)).await;
    assert!(connack.is_some(), "Must receive CONNACK");
    let (flags, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "Reason code must be 0x00 (Success)");
    assert_eq!(
        flags & 0x01,
        1,
        "[MQTT-3.1.4-4] Session present flag must be 1 when resuming existing session"
    );
}

/// `[MQTT-3.1.4-5]` If the Will Flag is set to 1, the Server MUST store the
/// Will Message and publish the Will Message after the Network Connection is
/// subsequently closed unless the Will Message has been deleted on receipt of
/// a DISCONNECT packet with Reason Code 0x00.
///
/// Connects a publisher with a Will Message, then disconnects abnormally.
/// A subscriber verifies the Will Message is delivered.
#[tokio::test]
async fn connect_will_published_on_abnormal_disconnect() {
    let broker = ConformanceBroker::start().await;
    let will_topic = format!("will/{}", unique_client_id("abnormal"));

    let collector = MessageCollector::new();
    let subscriber = connected_client("will-sub", &broker).await;
    subscriber
        .subscribe(&will_topic, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let will = WillMessage::new(&will_topic, b"offline".to_vec());
    let opts = ConnectOptions::new(unique_client_id("will-pub"))
        .with_clean_start(true)
        .with_will(will);
    let publisher = MqttClient::with_options(opts.clone());
    Box::pin(publisher.connect_with_options(broker.address(), opts))
        .await
        .expect("connect failed");

    publisher
        .disconnect_abnormally()
        .await
        .expect("abnormal disconnect failed");
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        collector.wait_for_messages(1, Duration::from_secs(3)).await,
        "[MQTT-3.1.4-5] Will message must be published on abnormal disconnect"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].topic, will_topic);
    assert_eq!(msgs[0].payload, b"offline");

    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.4-5]` (negative case) Will Message MUST NOT be published when
/// the client sends a normal DISCONNECT with Reason Code 0x00.
///
/// Connects a publisher with a Will Message, disconnects normally, and
/// verifies no Will Message is delivered to the subscriber.
#[tokio::test]
async fn connect_will_not_published_on_normal_disconnect() {
    let broker = ConformanceBroker::start().await;
    let will_topic = format!("will/{}", unique_client_id("normal"));

    let collector = MessageCollector::new();
    let subscriber = connected_client("will-sub2", &broker).await;
    subscriber
        .subscribe(&will_topic, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let will = WillMessage::new(&will_topic, b"offline".to_vec());
    let opts = ConnectOptions::new(unique_client_id("will-pub2"))
        .with_clean_start(true)
        .with_will(will);
    let publisher = MqttClient::with_options(opts.clone());
    Box::pin(publisher.connect_with_options(broker.address(), opts))
        .await
        .expect("connect failed");

    publisher.disconnect().await.expect("disconnect failed");
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        collector.count().await,
        0,
        "[MQTT-3.1.4-5] Will message must NOT be published on normal disconnect"
    );

    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.4-6]` The Server MUST respond to a CONNECT packet with a CONNACK
/// packet. The CONNACK is the first packet sent from the Server to the Client.
///
/// Sends a valid CONNECT via raw client and verifies a CONNACK with reason
/// code Success (0x00) is received.
#[tokio::test]
async fn connect_success_connack() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("connack");
    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    let connack = raw.expect_connack(Duration::from_secs(2)).await;
    assert!(
        connack.is_some(),
        "[MQTT-3.1.4-6] Server must respond with CONNACK"
    );
    let (_, reason) = connack.unwrap();
    assert_eq!(
        reason, 0x00,
        "[MQTT-3.1.4-6] Valid CONNECT must receive Success (0x00)"
    );
}

fn find_connack_reason_code_index(data: &[u8]) -> usize {
    let mut idx = 1;
    loop {
        if idx >= data.len() {
            return idx;
        }
        let byte = data[idx];
        idx += 1;
        if byte & 0x80 == 0 {
            break;
        }
    }
    idx + 1
}

fn build_connect_with_flags(connect_flags: u8) -> Vec<u8> {
    use bytes::{BufMut, BytesMut};

    let mut body = BytesMut::new();
    body.put_u16(4);
    body.put_slice(b"MQTT");
    body.put_u8(5);
    body.put_u8(connect_flags);
    body.put_u16(60);
    body.put_u8(0);
    put_mqtt_string(&mut body, "test-flags-client");

    if connect_flags & 0x04 != 0 {
        body.put_u8(0);
        put_mqtt_string(&mut body, "will/topic");
        put_mqtt_string(&mut body, "will-payload");
    }

    wrap_fixed_header(0x10, &body)
}

fn build_connect_resume(client_id: &str, session_expiry: u32) -> Vec<u8> {
    use bytes::{BufMut, BytesMut};

    let mut body = BytesMut::new();
    body.put_u16(4);
    body.put_slice(b"MQTT");
    body.put_u8(5);
    body.put_u8(0x00);
    body.put_u16(60);

    let mut props = BytesMut::new();
    props.put_u8(0x11);
    props.put_u32(session_expiry);
    encode_variable_int(&mut body, props.len() as u32);
    body.put(props);

    put_mqtt_string(&mut body, client_id);

    wrap_fixed_header(0x10, &body)
}

fn put_mqtt_string(buf: &mut bytes::BytesMut, s: &str) {
    use bytes::BufMut;
    let bytes = s.as_bytes();
    buf.put_u16(bytes.len() as u16);
    buf.put_slice(bytes);
}

fn encode_variable_int(buf: &mut bytes::BytesMut, mut value: u32) {
    use bytes::BufMut;
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value > 0 {
            byte |= 0x80;
        }
        buf.put_u8(byte);
        if value == 0 {
            break;
        }
    }
}

fn wrap_fixed_header(first_byte: u8, body: &[u8]) -> Vec<u8> {
    use bytes::{BufMut, BytesMut};
    let mut packet = BytesMut::new();
    packet.put_u8(first_byte);
    encode_variable_int(&mut packet, body.len() as u32);
    packet.put_slice(body);
    packet.to_vec()
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
