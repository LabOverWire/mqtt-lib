//! Section 3.1 — CONNECT packet conformance tests.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{
    encode_variable_int, put_mqtt_string, wrap_fixed_header, RawMqttClient, RawPacketBuilder,
};
use crate::sut::SutHandle;
use crate::test_client::TestClient;
use mqtt5_protocol::types::{ConnectOptions, SubscribeOptions, WillMessage};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// `[MQTT-3.1.0-1]` After a Network Connection is established by a Client to
/// a Server, the first packet sent from the Client to the Server MUST be a
/// CONNECT packet.
#[conformance_test(
    ids = ["MQTT-3.1.0-1"],
    requires = ["transport.tcp"],
)]
async fn connect_first_packet_must_be_connect(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::publish_without_connect())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.1.0-1] Server must disconnect client that sends non-CONNECT first"
    );
}

/// `[MQTT-3.1.0-2]` The Server MUST process a second CONNECT packet sent from
/// a Client as a Protocol Error and close the Network Connection.
#[conformance_test(
    ids = ["MQTT-3.1.0-2"],
    requires = ["transport.tcp"],
)]
async fn connect_second_connect_is_protocol_error(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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
#[conformance_test(
    ids = ["MQTT-3.1.2-1"],
    requires = ["transport.tcp"],
)]
async fn connect_protocol_name_must_be_mqtt(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

/// `[MQTT-3.1.2-2]` The Server MUST respond to a CONNECT packet with a
/// CONNACK with 0x84 (Unsupported Protocol Version) if the Protocol Version
/// is not supported.
#[conformance_test(
    ids = ["MQTT-3.1.2-2"],
    requires = ["transport.tcp"],
)]
async fn connect_unsupported_protocol_version(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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
/// CONNECT packet is set to 0.
#[conformance_test(
    ids = ["MQTT-3.1.2-3"],
    requires = ["transport.tcp"],
)]
async fn connect_reserved_flag_must_be_zero(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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
#[conformance_test(
    ids = ["MQTT-3.1.2-4"],
    requires = ["transport.tcp"],
)]
async fn connect_clean_start_new_session(sut: SutHandle) {
    let client_id = unique_client_id("clean");

    let opts = ConnectOptions::new(&client_id)
        .with_clean_start(true)
        .with_session_expiry_interval(300);
    let client = TestClient::connect_with_options(&sut, opts)
        .await
        .expect("[MQTT-3.1.2-4] Clean start connection must succeed");
    assert!(
        !client.session_present(),
        "[MQTT-3.1.2-4] First connection with clean_start=true must have session_present=false"
    );
    client
        .subscribe("test/topic", SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    client.disconnect().await.expect("disconnect failed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let opts2 = ConnectOptions::new(&client_id)
        .with_clean_start(true)
        .with_session_expiry_interval(300);
    let client2 = TestClient::connect_with_options(&sut, opts2)
        .await
        .expect("reconnect failed");
    assert!(
        !client2.session_present(),
        "[MQTT-3.1.2-4] CleanStart=1 must discard existing session (session_present=false)"
    );
    client2.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.2-5]` If `CleanStart` is set to 0 and there is a Session
/// associated with the Client Identifier, the Server MUST resume
/// communications based on state from the existing Session.
#[conformance_test(
    ids = ["MQTT-3.1.2-5"],
    requires = ["transport.tcp"],
)]
async fn connect_clean_start_false_resumes_session(sut: SutHandle) {
    let client_id = unique_client_id("resume");

    let opts = ConnectOptions::new(&client_id)
        .with_clean_start(true)
        .with_session_expiry_interval(300);
    let client = TestClient::connect_with_options(&sut, opts)
        .await
        .expect("first connect failed");
    client
        .subscribe("test/resume", SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    client.disconnect().await.expect("disconnect failed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let opts2 = ConnectOptions::new(&client_id)
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let client2 = TestClient::connect_with_options(&sut, opts2)
        .await
        .expect("reconnect failed");
    assert!(
        client2.session_present(),
        "[MQTT-3.1.2-5] CleanStart=0 with existing session must have session_present=true"
    );
    client2.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.2-6]` If the Will Flag is set to 1, the Will Properties, Will
/// Topic and Will Payload fields MUST be present in the Payload.
#[conformance_test(
    ids = ["MQTT-3.1.2-6"],
    requires = ["transport.tcp"],
)]
async fn connect_will_flag_with_will_topic_payload(sut: SutHandle) {
    let will = WillMessage::new("will/topic", b"will-payload".to_vec());
    let opts = ConnectOptions::new(unique_client_id("will"))
        .with_clean_start(true)
        .with_will(will);
    let client = TestClient::connect_with_options(&sut, opts)
        .await
        .expect("[MQTT-3.1.2-6] Connection with valid will must succeed");
    client.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.2-7]` If the Will Flag is set to 0, then the Will `QoS` MUST
/// be set to 0.
#[conformance_test(
    ids = ["MQTT-3.1.2-7"],
    requires = ["transport.tcp"],
)]
async fn connect_will_qos_zero_without_will_flag(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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
#[conformance_test(
    ids = ["MQTT-3.1.2-8"],
    requires = ["transport.tcp"],
)]
async fn connect_will_retain_zero_without_will_flag(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    let packet = build_connect_with_flags(0x22);
    raw.send_raw(&packet).await.unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(2)).await,
        "[MQTT-3.1.2-8] Server must reject Will Retain=1 when Will Flag=0"
    );
}

/// `[MQTT-3.1.2-9]` If the Will Flag is set to 1, the value of Will `QoS`
/// can be 0, 1, or 2. A value of 3 is a Malformed Packet.
#[conformance_test(
    ids = ["MQTT-3.1.2-9"],
    requires = ["transport.tcp"],
)]
async fn connect_will_qos_3_is_malformed(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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
#[conformance_test(
    ids = ["MQTT-3.1.2-12"],
    requires = ["transport.tcp"],
)]
async fn connect_invalid_fixed_header_flags(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

/// `[MQTT-3.1.3-3]` The Server MUST allow `ClientID`s which are between 1
/// and 23 UTF-8 encoded bytes in length, and that contain only the
/// characters `0-9`, `a-z`, `A-Z`.
#[conformance_test(
    ids = ["MQTT-3.1.3-3"],
    requires = ["transport.tcp"],
)]
async fn connect_valid_client_id_accepted(sut: SutHandle) {
    let client = TestClient::connect(&sut, "abcABC012345")
        .await
        .expect("[MQTT-3.1.3-3] Server must accept valid client ID");
    client.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.3-4]` A Server MAY allow a Client to supply a `ClientID` that
/// has a length of zero bytes; if so the Server MUST treat this as a special
/// case and assign a unique `ClientID` to that Client.
#[conformance_test(
    ids = ["MQTT-3.1.3-4"],
    requires = ["transport.tcp"],
)]
async fn connect_empty_client_id_server_assigns(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

/// `[MQTT-3.1.3-5]` If the Server rejects the `ClientID` it MAY respond to
/// the CONNECT packet with a CONNACK using Reason Code 0x85
/// (`ClientIdentifierNotValid`).
#[conformance_test(
    ids = ["MQTT-3.1.3-5"],
    requires = ["transport.tcp"],
)]
async fn client_id_rejected_with_0x85(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::valid_connect("bad/id"))
        .await
        .unwrap();

    let connack = raw.expect_connack(Duration::from_secs(2)).await;
    assert!(connack.is_some(), "[MQTT-3.1.3-5] Server must send CONNACK");
    let (_, reason) = connack.unwrap();
    assert_eq!(
        reason, 0x85,
        "[MQTT-3.1.3-5] Server must reject invalid client ID with 0x85 (ClientIdentifierNotValid)"
    );
}

/// `[MQTT-3.1.4-1]` The Server MUST validate that the CONNECT packet matches
/// the format described in the specification and close the Network
/// Connection if it does not.
#[conformance_test(
    ids = ["MQTT-3.1.4-1"],
    requires = ["transport.tcp"],
)]
async fn connect_malformed_packet_closes_connection(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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
#[conformance_test(
    ids = ["MQTT-3.1.4-2"],
    requires = ["transport.tcp"],
)]
async fn connect_duplicate_property_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

/// `[MQTT-3.1.4-3]` If the Server accepts a connection with `CleanStart` set
/// to 1, the Server MUST set Session Present to 0 in the CONNACK packet.
#[conformance_test(
    ids = ["MQTT-3.1.4-3"],
    requires = ["transport.tcp"],
)]
async fn connect_clean_start_session_present_zero(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

/// `[MQTT-3.1.4-4]` If the Server accepts a connection with `CleanStart` set
/// to 0 and the Server has Session State for the `ClientID`, it MUST set
/// Session Present to 1 in the CONNACK packet.
#[conformance_test(
    ids = ["MQTT-3.1.4-4"],
    requires = ["transport.tcp"],
)]
async fn connect_clean_start_false_session_present_one(sut: SutHandle) {
    let client_id = unique_client_id("sp1");

    let opts = ConnectOptions::new(&client_id)
        .with_clean_start(true)
        .with_session_expiry_interval(300);
    let client = TestClient::connect_with_options(&sut, opts)
        .await
        .expect("first connect failed");
    client
        .subscribe("test/sp1", SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    client.disconnect().await.expect("disconnect failed");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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
/// Will Message and publish it after the Network Connection is subsequently
/// closed unless the Will Message has been deleted on receipt of a DISCONNECT
/// with Reason Code 0x00.
#[conformance_test(
    ids = ["MQTT-3.1.4-5"],
    requires = ["transport.tcp"],
)]
async fn connect_will_published_on_abnormal_disconnect(sut: SutHandle) {
    let will_topic = format!("will/{}", unique_client_id("abnormal"));

    let subscriber = TestClient::connect_with_prefix(&sut, "will-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&will_topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let will = WillMessage::new(&will_topic, b"offline".to_vec());
    let opts = ConnectOptions::new(unique_client_id("will-pub"))
        .with_clean_start(true)
        .with_will(will);
    let publisher = TestClient::connect_with_options(&sut, opts)
        .await
        .expect("connect failed");

    publisher
        .disconnect_abnormally()
        .await
        .expect("abnormal disconnect failed");

    assert!(
        subscription
            .wait_for_messages(1, Duration::from_secs(3))
            .await,
        "[MQTT-3.1.4-5] Will message must be published on abnormal disconnect"
    );
    let msgs = subscription.snapshot();
    assert_eq!(msgs[0].topic, will_topic);
    assert_eq!(msgs[0].payload, b"offline");

    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.4-5]` (negative case) Will Message MUST NOT be published when
/// the client sends a normal DISCONNECT with Reason Code 0x00.
#[conformance_test(
    ids = ["MQTT-3.1.4-5"],
    requires = ["transport.tcp"],
)]
async fn connect_will_not_published_on_normal_disconnect(sut: SutHandle) {
    let will_topic = format!("will/{}", unique_client_id("normal"));

    let subscriber = TestClient::connect_with_prefix(&sut, "will-sub2")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&will_topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let will = WillMessage::new(&will_topic, b"offline".to_vec());
    let opts = ConnectOptions::new(unique_client_id("will-pub2"))
        .with_clean_start(true)
        .with_will(will);
    let publisher = TestClient::connect_with_options(&sut, opts)
        .await
        .expect("connect failed");

    publisher.disconnect().await.expect("disconnect failed");
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        subscription.count(),
        0,
        "[MQTT-3.1.4-5] Will message must NOT be published on normal disconnect"
    );

    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.4-6]` The Server MUST respond to a CONNECT packet with a
/// CONNACK packet. The CONNACK is the first packet sent from the Server to
/// the Client.
#[conformance_test(
    ids = ["MQTT-3.1.4-6"],
    requires = ["transport.tcp"],
)]
async fn connect_success_connack(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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
    encode_variable_int(&mut body, u32::try_from(props.len()).unwrap());
    body.put(props);

    put_mqtt_string(&mut body, client_id);

    wrap_fixed_header(0x10, &body)
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
