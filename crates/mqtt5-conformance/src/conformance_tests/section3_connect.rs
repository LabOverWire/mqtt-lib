//! Section 3.1 — CONNECT packet conformance tests.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{put_mqtt_string, wrap_fixed_header, RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use crate::test_client::TestClient;
use mqtt5_protocol::types::{ConnectOptions, SubscribeOptions, WillMessage};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// `[MQTT-3.1.4-3]` If the `ClientID` represents a Client already connected to
/// the Server, the Server sends a DISCONNECT packet to the existing Client
/// with Reason Code of 0x8E (Session taken over) and MUST close the Network
/// Connection of the existing Client.
#[conformance_test(
    ids = ["MQTT-3.1.4-3"],
    requires = ["transport.tcp"],
)]
async fn session_takeover_disconnects_existing_client(sut: SutHandle) {
    let client_id = unique_client_id("takeover");

    let mut existing = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    existing
        .send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();
    let (_, reason) = existing
        .expect_connack(TIMEOUT)
        .await
        .expect("first connection must be accepted");
    assert_eq!(reason, 0x00, "first CONNECT must succeed");

    let mut taking_over = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    taking_over
        .send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();
    let (_, reason) = taking_over
        .expect_connack(TIMEOUT)
        .await
        .expect("second connection with the same ClientID must be accepted");
    assert_eq!(reason, 0x00, "taking-over CONNECT must succeed");

    let reason = existing
        .expect_disconnect_packet(TIMEOUT)
        .await
        .expect("[MQTT-3.1.4-3] the superseded client must receive a DISCONNECT before the connection closes");
    assert_eq!(
        reason, 0x8E,
        "[MQTT-3.1.4-3] the DISCONNECT sent to the superseded client must carry 0x8E (Session taken over)"
    );

    assert!(
        existing.read_packet_bytes(TIMEOUT).await.is_none(),
        "[MQTT-3.1.4-3] Server must close the Network Connection of the client whose session was taken over; \
         it is still open and readable"
    );
}

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

    if let Some(data) = raw.read_packet_bytes(Duration::from_secs(2)).await {
        assert_eq!(
            data[0], 0x20,
            "[MQTT-3.1.2-2] the only packet permitted before closing is a CONNACK"
        );
        let reason_idx = find_connack_reason_code_index(&data);
        assert_eq!(
            data[reason_idx], 0x84,
            "[MQTT-3.1.2-2] a CONNACK sent here must carry Reason Code 0x84 (Unsupported Protocol Version)"
        );
    }

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.1.2-2] Server MUST close the Network Connection; sending the CONNACK first is only a MAY"
    );
}

/// `[MQTT-3.1.2-3]` `[MQTT-3.1.4-1]` The Server MUST validate that the reserved
/// flag in the CONNECT packet is 0, and close the connection when it is not.
#[conformance_test(
    ids = ["MQTT-3.1.2-3", "MQTT-3.1.4-1"],
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

/// `[MQTT-3.2.2-2]` A connection accepted with Clean Start 1 must get Session
/// Present 0, both on the first connection and on a reconnect to an existing
/// session.
#[conformance_test(
    ids = ["MQTT-3.2.2-2"],
    requires = ["transport.tcp"],
)]
async fn connect_clean_start_new_session(sut: SutHandle) {
    let client_id = unique_client_id("clean");

    let opts = ConnectOptions::new(&client_id)
        .with_clean_start(true)
        .with_session_expiry_interval(300);
    let client = TestClient::connect_with_options(&sut, opts)
        .await
        .expect("Clean start connection must succeed");
    assert!(
        !client.session_present(),
        "[MQTT-3.2.2-2] First connection with clean_start=true must have session_present=false"
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
        "[MQTT-3.2.2-2] CleanStart=1 must discard existing session (session_present=false)"
    );
    client2.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.2.2-3]` Reconnecting with Clean Start 0 to an existing session must
/// get Session Present 1.
#[conformance_test(
    ids = ["MQTT-3.2.2-3"],
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
        "[MQTT-3.2.2-3] CleanStart=0 with existing session must have session_present=true"
    );
    client2.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.4-5]` A CONNECT with the Will Flag set and the Will Properties,
/// Will Topic and Will Payload present must be acknowledged with a CONNACK
/// 0x00.
#[conformance_test(
    ids = ["MQTT-3.1.4-5"],
    requires = ["transport.tcp"],
)]
async fn connect_will_flag_with_will_topic_payload(sut: SutHandle) {
    let will = WillMessage::new("will/topic", b"will-payload".to_vec());
    let opts = ConnectOptions::new(unique_client_id("will"))
        .with_clean_start(true)
        .with_will(will);
    let client = TestClient::connect_with_options(&sut, opts)
        .await
        .expect("[MQTT-3.1.4-5] Connection with valid will must succeed");
    client.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.2-11]` `[MQTT-3.1.4-1]` If the Will Flag is 0, the Will `QoS`
/// MUST be 0. A CONNECT with Will `QoS` 1 and no Will Flag does not match the
/// format of section 3.1, so the Server must close the connection.
#[conformance_test(
    ids = ["MQTT-3.1.2-11", "MQTT-3.1.4-1"],
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
        "[MQTT-3.1.2-11] Server must reject Will QoS!=0 when Will Flag=0"
    );
}

/// `[MQTT-3.1.2-13]` `[MQTT-3.1.4-1]` If the Will Flag is 0, Will Retain MUST
/// be 0. A CONNECT with Will Retain set and no Will Flag does not match the
/// format of section 3.1, so the Server must close the connection.
#[conformance_test(
    ids = ["MQTT-3.1.2-13", "MQTT-3.1.4-1"],
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
        "[MQTT-3.1.2-13] Server must reject Will Retain=1 when Will Flag=0"
    );
}

/// `[MQTT-3.1.2-12]` `[MQTT-3.1.4-1]` Will `QoS` can be 0, 1 or 2. A CONNECT
/// with Will `QoS` 3 does not match the format of section 3.1, so the Server
/// must close the connection.
#[conformance_test(
    ids = ["MQTT-3.1.2-12", "MQTT-3.1.4-1"],
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
        "[MQTT-3.1.2-12] Server must reject Will QoS=3 as malformed"
    );
}

/// `[MQTT-2.1.3-1]` `[MQTT-3.1.4-1]` The CONNECT fixed header flags are
/// reserved and MUST be 0. A CONNECT with any other value does not match the
/// format of section 3.1, so the Server must close the connection.
#[conformance_test(
    ids = ["MQTT-2.1.3-1", "MQTT-3.1.4-1"],
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
        "[MQTT-2.1.3-1] Server must reject CONNECT with non-zero fixed header flags"
    );
}

/// `[MQTT-3.1.3-5]` The Server MUST allow `ClientID`s which are between 1
/// and 23 UTF-8 encoded bytes in length, and that contain only the
/// characters `0-9`, `a-z`, `A-Z`.
#[conformance_test(
    ids = ["MQTT-3.1.3-5"],
    requires = ["transport.tcp"],
)]
async fn connect_valid_client_id_accepted(sut: SutHandle) {
    let client = TestClient::connect(&sut, "abcABC012345")
        .await
        .expect("[MQTT-3.1.3-5] Server must accept valid client ID");
    client.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.3-6]` `[MQTT-3.1.3-7]` `[MQTT-3.2.2-16]` A Server that accepts a
/// zero-length `ClientID` MUST assign a unique `ClientID`, process the CONNECT
/// as if the Client had sent it, and return it as the Assigned Client
/// Identifier in the CONNACK.
#[conformance_test(
    ids = ["MQTT-3.1.3-6", "MQTT-3.1.3-7", "MQTT-3.2.2-16"],
    requires = ["transport.tcp", "assigned_client_id_supported"],
)]
async fn connect_empty_client_id_server_assigns(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    let connect = build_connect_empty_client_id();
    raw.send_raw(&connect).await.unwrap();

    let response = raw.read_packet_bytes(Duration::from_secs(2)).await;
    assert!(response.is_some(), "Server must send CONNACK");
    let data = response.unwrap();
    assert_eq!(data[0], 0x20, "Response must be CONNACK");
    let reason_idx = find_connack_reason_code_index(&data);
    assert_eq!(
        data[reason_idx], 0x00,
        "[MQTT-3.1.3-6] Empty client ID with clean_start must be accepted (reason=Success)"
    );
    assert!(
        data[reason_idx + 1..].contains(&0x12),
        "[MQTT-3.1.3-7] CONNACK must contain Assigned Client Identifier property (0x12)"
    );
}

/// `[MQTT-3.1.3-8]` If the Server rejects the `ClientID` it MAY respond to
/// the CONNECT packet with a CONNACK using Reason Code 0x85
/// (`ClientIdentifierNotValid`).
#[conformance_test(
    ids = ["MQTT-3.1.3-8"],
    requires = ["transport.tcp", "strict_client_id_charset"],
)]
async fn client_id_rejected_with_0x85(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::valid_connect("bad/id"))
        .await
        .unwrap();

    let connack = raw.expect_connack(Duration::from_secs(2)).await;
    assert!(connack.is_some(), "[MQTT-3.1.3-8] Server must send CONNACK");
    let (_, reason) = connack.unwrap();
    assert_eq!(
        reason, 0x85,
        "[MQTT-3.1.3-8] Server must reject invalid client ID with 0x85 (ClientIdentifierNotValid)"
    );
}

/// `[MQTT-3.1.4-1]` A CONNECT whose properties length runs past the end of the
/// packet does not match the format of section 3.1, so the Server must close
/// the connection.
#[conformance_test(
    ids = ["MQTT-3.1.4-1"],
    requires = ["transport.tcp"],
)]
async fn connect_malformed_packet_closes_connection(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_overlong_properties())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.1.4-1] Server must close connection on malformed CONNECT"
    );
}

/// `[MQTT-3.1.4-1]` A CONNECT that includes the Session Expiry Interval
/// property twice does not match the format of section 3.1, so the Server must
/// close the connection.
#[conformance_test(
    ids = ["MQTT-3.1.4-1"],
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
        "[MQTT-3.1.4-1] Server must reject CONNECT with duplicate properties"
    );
}

/// `[MQTT-3.2.2-2]` If the Server accepts a connection with `CleanStart` set
/// to 1, the Server MUST set Session Present to 0 in the CONNACK packet.
#[conformance_test(
    ids = ["MQTT-3.2.2-2"],
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
        "[MQTT-3.2.2-2] Reason code must be 0x00 (Success)"
    );
    assert_eq!(
        flags & 0x01,
        0,
        "[MQTT-3.2.2-2] Session present flag must be 0 for clean start"
    );
}

/// `[MQTT-3.2.2-2]` Clean Start = 1 taking over a connected Client's session
/// must still report Session Present = 0.
#[conformance_test(
    ids = ["MQTT-3.2.2-2"],
    requires = ["transport.tcp"],
)]
async fn connect_clean_start_takeover_session_present_zero(sut: SutHandle) {
    let client_id = unique_client_id("sp0-takeover");

    let mut existing = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    existing
        .send_raw(&RawPacketBuilder::connect_with_session_expiry(
            &client_id, 300,
        ))
        .await
        .unwrap();
    existing
        .expect_connack(TIMEOUT)
        .await
        .expect("first connect must receive CONNACK");
    existing
        .send_raw(&RawPacketBuilder::subscribe_with_packet_id(
            &format!("sp0-takeover/{client_id}"),
            0,
            1,
        ))
        .await
        .unwrap();
    let (_, reason_codes) = existing
        .expect_suback(TIMEOUT)
        .await
        .expect("first connection must receive SUBACK");
    assert_eq!(
        reason_codes,
        [0x00],
        "the session being taken over must hold a subscription"
    );

    let mut taker = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    taker
        .send_raw(&RawPacketBuilder::connect_with_session_expiry(
            &client_id, 300,
        ))
        .await
        .unwrap();
    let (flags, reason) = taker
        .expect_connack(TIMEOUT)
        .await
        .expect("takeover connect must receive CONNACK");
    assert_eq!(
        reason, 0x00,
        "[MQTT-3.2.2-2] Reason code must be 0x00 (Success)"
    );
    assert_eq!(
        flags & 0x01,
        0,
        "[MQTT-3.2.2-2] Session Present must be 0 when Clean Start = 1 takes over a live session"
    );
}

/// `[MQTT-3.2.2-3]` If the Server accepts a connection with Clean Start 0 and
/// has Session State for the `ClientID`, it MUST set Session Present to 1 in
/// the CONNACK.
#[conformance_test(
    ids = ["MQTT-3.2.2-3"],
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
    raw.send_raw(&RawPacketBuilder::connect_resume(&client_id, 300))
        .await
        .unwrap();

    let connack = raw.expect_connack(Duration::from_secs(2)).await;
    assert!(connack.is_some(), "Must receive CONNACK");
    let (flags, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "Reason code must be 0x00 (Success)");
    assert_eq!(
        flags & 0x01,
        1,
        "[MQTT-3.2.2-3] Session present flag must be 1 when resuming existing session"
    );
}

/// `[MQTT-3.1.2-8]` The Will Message MUST be published after the Network
/// Connection is closed without a DISCONNECT.
#[conformance_test(
    ids = ["MQTT-3.1.2-8"],
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
        "[MQTT-3.1.2-8] Will message must be published on abnormal disconnect"
    );
    let msgs = subscription.snapshot();
    assert_eq!(msgs[0].topic, will_topic);
    assert_eq!(msgs[0].payload, b"offline");

    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.14.4-3]` On a DISCONNECT with Reason Code 0x00 the Server MUST
/// discard the Will Message without publishing it.
#[conformance_test(
    ids = ["MQTT-3.14.4-3"],
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
        "[MQTT-3.14.4-3] Will message must NOT be published on normal disconnect"
    );

    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.4-5]` `[MQTT-3.2.0-1]` The Server MUST acknowledge a valid
/// CONNECT with a CONNACK 0x00, and the CONNACK must be the first packet it
/// sends.
#[conformance_test(
    ids = ["MQTT-3.1.4-5", "MQTT-3.2.0-1"],
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
        "[MQTT-3.1.4-5] Server must respond with CONNACK"
    );
    let (_, reason) = connack.unwrap();
    assert_eq!(
        reason, 0x00,
        "[MQTT-3.1.4-5] Valid CONNECT must receive Success (0x00)"
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
    put_mqtt_string(&mut body, &unique_client_id("flags"));

    if connect_flags & 0x04 != 0 {
        body.put_u8(0);
        put_mqtt_string(&mut body, "will/topic");
        put_mqtt_string(&mut body, "will-payload");
    }

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
