//! Section 3.1 — Extended CONNECT conformance tests.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{put_mqtt_string, wrap_fixed_header, RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use crate::test_client::TestClient;
use mqtt5_protocol::types::{ConnectOptions, SubscribeOptions};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// `[MQTT-3.1.2-14]` If Will Retain is set to 0, the Server MUST publish the
/// Will Message as a non-retained message.
#[conformance_test(
    ids = ["MQTT-3.1.2-14"],
    requires = ["transport.tcp"],
)]
async fn will_retain_zero_publishes_non_retained(sut: SutHandle) {
    let client_id = unique_client_id("wr0");
    let will_topic = format!("will/{client_id}");

    let subscriber = TestClient::connect_with_prefix(&sut, "wr0-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&will_topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::connect_with_will_retain_flag(
        &client_id, false,
    ))
    .await
    .unwrap();
    let connack = raw.expect_connack(TIMEOUT).await;
    assert!(connack.is_some(), "Must receive CONNACK");
    let (_, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "Connection must succeed");

    drop(raw);
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(
        subscription
            .wait_for_messages(1, Duration::from_secs(5))
            .await,
        "[MQTT-3.1.2-14] Will message must be published on connection drop"
    );
    let msgs = subscription.snapshot();
    assert!(
        !msgs[0].retain,
        "[MQTT-3.1.2-14] Will message with Will Retain=0 must be published as non-retained"
    );

    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.2-15]` If Will Retain is set to 1, the Server MUST publish the
/// Will Message as a retained message.
#[conformance_test(
    ids = ["MQTT-3.1.2-15"],
    requires = ["transport.tcp", "retain_available"],
)]
async fn will_retain_one_publishes_retained(sut: SutHandle) {
    let client_id = unique_client_id("wr1");
    let will_topic = format!("will/{client_id}");

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::connect_with_will_retain_flag(
        &client_id, true,
    ))
    .await
    .unwrap();
    let connack = raw.expect_connack(TIMEOUT).await;
    assert!(connack.is_some(), "Must receive CONNACK");
    let (_, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "Connection must succeed");

    drop(raw);
    tokio::time::sleep(Duration::from_secs(1)).await;

    let subscriber = TestClient::connect_with_prefix(&sut, "wr1-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&will_topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    assert!(
        subscription
            .wait_for_messages(1, Duration::from_secs(5))
            .await,
        "[MQTT-3.1.2-15] New subscriber must receive retained will message"
    );
    let msgs = subscription.snapshot();
    assert!(
        msgs[0].retain,
        "[MQTT-3.1.2-15] Will message with Will Retain=1 must be delivered as retained"
    );

    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.2-16]` If the User Name Flag is set to 1, a User Name MUST be
/// present in the Payload.
#[conformance_test(
    ids = ["MQTT-3.1.2-16"],
    requires = ["transport.tcp"],
)]
async fn username_flag_with_username_succeeds(sut: SutHandle) {
    let client_id = unique_client_id("uflag-ok");
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::connect_with_username(
        &client_id, "testuser",
    ))
    .await
    .unwrap();
    let connack = raw.expect_connack(TIMEOUT).await;
    assert!(
        connack.is_some(),
        "[MQTT-3.1.2-16] CONNECT with Username Flag=1 and username present must succeed"
    );
    let (_, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "[MQTT-3.1.2-16] Reason code must be Success");
}

/// `[MQTT-3.1.2-17]` If the User Name Flag is set to 1 but no Username is
/// present in the payload (truncated), the Server MUST treat it as malformed.
#[conformance_test(
    ids = ["MQTT-3.1.2-17"],
    requires = ["transport.tcp"],
)]
async fn username_flag_without_username_is_malformed(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&build_connect_username_flag_set_no_username())
        .await
        .unwrap();
    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.1.2-17] Server must disconnect when Username Flag=1 but no username in payload"
    );
}

/// `[MQTT-3.1.2-18]` `[MQTT-3.1.2-19]` If the Password Flag is set to 1, a
/// Password MUST be present in the Payload.
#[conformance_test(
    ids = ["MQTT-3.1.2-18", "MQTT-3.1.2-19"],
    requires = ["transport.tcp"],
)]
async fn password_flag_payload_consistency(sut: SutHandle) {
    let client_id = unique_client_id("pflag-ok");
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&build_connect_with_username_and_password(
        &client_id, "testuser", "secret",
    ))
    .await
    .unwrap();
    let connack = raw.expect_connack(TIMEOUT).await;
    assert!(
        connack.is_some(),
        "[MQTT-3.1.2-18] CONNECT with Password Flag=1 and password present must succeed"
    );
    let (_, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "[MQTT-3.1.2-18] Reason code must be Success");
}

/// `[MQTT-3.1.2-23]` If Session Expiry Interval is greater than 0, the Server
/// MUST store the Session State after the Network Connection is closed.
#[conformance_test(
    ids = ["MQTT-3.1.2-23"],
    requires = ["transport.tcp"],
)]
async fn session_stored_when_expiry_positive(sut: SutHandle) {
    let client_id = unique_client_id("sess-store");
    let topic = format!("sess/{client_id}");

    let opts = ConnectOptions::new(&client_id)
        .with_clean_start(true)
        .with_session_expiry_interval(300);
    let client1 = TestClient::connect_with_options(&sut, opts)
        .await
        .expect("first connect failed");
    client1
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    client1.disconnect().await.expect("disconnect failed");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let opts2 = ConnectOptions::new(&client_id)
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let client2 = TestClient::connect_with_options(&sut, opts2)
        .await
        .expect("reconnect failed");
    assert!(
        client2.session_present(),
        "[MQTT-3.1.2-23] Reconnect with clean_start=false must have session_present=true"
    );

    let subscription = client2
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("re-subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "sess-pub")
        .await
        .unwrap();
    publisher
        .publish(&topic, b"hello")
        .await
        .expect("publish failed");

    assert!(
        subscription
            .wait_for_messages(1, Duration::from_secs(3))
            .await,
        "[MQTT-3.1.2-23] Subscription must survive across session reconnect"
    );

    client2.disconnect().await.expect("disconnect failed");
    publisher.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.2-24]` `[MQTT-3.1.2-25]` The Server MUST NOT send packets
/// exceeding the Maximum Packet Size declared by the Client.
#[conformance_test(
    ids = ["MQTT-3.1.2-24", "MQTT-3.1.2-25"],
    requires = ["transport.tcp"],
)]
async fn server_discards_oversized_publish(sut: SutHandle) {
    let client_id = unique_client_id("maxpkt");
    let topic = "t/mp";

    let mut opts = ConnectOptions::new(&client_id).with_clean_start(true);
    opts.properties.maximum_packet_size = Some(128);
    let subscriber = TestClient::connect_with_options(&sut, opts)
        .await
        .expect("connect failed");
    let subscription = subscriber
        .subscribe(topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "maxpkt-pub")
        .await
        .unwrap();

    let large_payload = vec![b'X'; 200];
    publisher
        .publish(topic, &large_payload)
        .await
        .expect("publish failed");
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert_eq!(
        subscription.count(),
        0,
        "[MQTT-3.1.2-24] Server must not send packet exceeding client Maximum Packet Size"
    );

    publisher
        .publish(topic, b"ok")
        .await
        .expect("small publish failed");

    assert!(
        subscription
            .wait_for_messages(1, Duration::from_secs(3))
            .await,
        "[MQTT-3.1.2-25] Server must still deliver packets within Maximum Packet Size"
    );
    let msgs = subscription.snapshot();
    assert_eq!(msgs[0].payload, b"ok");

    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.2-28]` If the Client sets Request Response Information to 0,
/// the Server MUST NOT include Response Information in the CONNACK.
#[conformance_test(
    ids = ["MQTT-3.1.2-28"],
    requires = ["transport.tcp"],
)]
async fn request_response_info_zero_suppresses(sut: SutHandle) {
    let client_id = unique_client_id("rri0");

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::connect_with_request_response_info(
        &client_id, 0,
    ))
    .await
    .unwrap();

    let connack = raw
        .expect_connack_packet(TIMEOUT)
        .await
        .expect("Must receive CONNACK");

    assert_eq!(
        connack.reason_code,
        mqtt5_protocol::protocol::v5::reason_codes::ReasonCode::Success
    );
    assert!(
        connack
            .properties
            .get(mqtt5_protocol::protocol::v5::properties::PropertyId::ResponseInformation)
            .is_none(),
        "[MQTT-3.1.2-28] CONNACK must not contain Response Information when Request Response Information=0"
    );
}

/// `[MQTT-3.1.3-8]` If the Server rejects the `ClientID`, it sends CONNACK
/// with 0x85 and MUST then close the Network Connection.
#[conformance_test(
    ids = ["MQTT-3.1.3-8"],
    requires = ["transport.tcp", "strict_client_id_charset"],
)]
async fn client_id_rejected_closes_connection(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::valid_connect("bad/id"))
        .await
        .unwrap();

    let response = raw.read_packet_bytes(TIMEOUT).await;
    assert!(response.is_some(), "Must receive CONNACK");
    let data = response.unwrap();
    assert_eq!(data[0], 0x20, "Response must be CONNACK");

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.1.3-8] Server must close connection after rejecting client ID with 0x85"
    );
}

/// `[MQTT-3.1.3-9]` If a new Network Connection to this Session is made
/// before the Will Delay Interval has passed, the Server MUST NOT send the
/// Will Message.
#[conformance_test(
    ids = ["MQTT-3.1.3-9"],
    requires = ["transport.tcp"],
)]
async fn will_delay_reconnect_suppresses_will(sut: SutHandle) {
    let client_id = unique_client_id("wdel");
    let will_topic = format!("will/{client_id}");

    let subscriber = TestClient::connect_with_prefix(&sut, "wdel-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&will_topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::connect_with_will_delay(
        &client_id, 5, 60,
    ))
    .await
    .unwrap();
    let connack = raw.expect_connack(TIMEOUT).await;
    assert!(connack.is_some(), "Must receive CONNACK");
    let (_, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "Connection must succeed");

    drop(raw);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let opts = ConnectOptions::new(&client_id)
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let reconnected = TestClient::connect_with_options(&sut, opts)
        .await
        .expect("reconnect failed");

    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        subscription.count(),
        0,
        "[MQTT-3.1.3-9] Will message must not be sent when client reconnects before will delay expires"
    );

    reconnected.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.1.3-10]` The User Property is part of the Will Properties and
/// the Server MUST maintain the order of User Properties when publishing the
/// Will Message.
#[conformance_test(
    ids = ["MQTT-3.1.3-10"],
    requires = ["transport.tcp"],
)]
async fn will_user_property_order_preserved(sut: SutHandle) {
    let client_id = unique_client_id("wup");
    let will_topic = format!("will/{client_id}");

    let subscriber = TestClient::connect_with_prefix(&sut, "wup-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&will_topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let user_props = [("key1", "val1"), ("key2", "val2"), ("key3", "val3")];
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::connect_with_will_user_properties(
        &client_id,
        &user_props,
    ))
    .await
    .unwrap();
    let connack = raw.expect_connack(TIMEOUT).await;
    assert!(connack.is_some(), "Must receive CONNACK");
    let (_, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "Connection must succeed");

    drop(raw);
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(
        subscription
            .wait_for_messages(1, Duration::from_secs(5))
            .await,
        "[MQTT-3.1.3-10] Will message must be published on connection drop"
    );
    let msgs = subscription.snapshot();
    let received_props: Vec<(&str, &str)> = msgs[0]
        .user_properties
        .iter()
        .filter(|(k, _)| k.starts_with("key"))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        received_props,
        vec![("key1", "val1"), ("key2", "val2"), ("key3", "val3")],
        "[MQTT-3.1.3-10] Will user properties must be delivered in the same order"
    );

    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-4.1.0-1]` The Client and Server MUST store Session State for the
/// entire duration of the Session.
#[conformance_test(
    ids = ["MQTT-4.1.0-1"],
    requires = ["transport.tcp"],
)]
async fn session_not_discarded_while_connected(sut: SutHandle) {
    let topic = format!("sess/{}", unique_client_id("active"));

    let client = TestClient::connect_with_prefix(&sut, "sess-active")
        .await
        .unwrap();
    let subscription = client
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    client
        .publish(&topic, b"alive")
        .await
        .expect("publish failed");

    assert!(
        subscription
            .wait_for_messages(1, Duration::from_secs(3))
            .await,
        "[MQTT-4.1.0-1] Session state (subscriptions) must persist while connected"
    );
    let msgs = subscription.snapshot();
    assert_eq!(msgs[0].payload, b"alive");

    client.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-4.1.0-2]` After the Session Expiry Interval has passed, the Server
/// MUST discard the Session State.
#[conformance_test(
    ids = ["MQTT-4.1.0-2"],
    requires = ["transport.tcp"],
)]
async fn session_discarded_after_expiry(sut: SutHandle) {
    let client_id = unique_client_id("sess-exp");

    let opts = ConnectOptions::new(&client_id)
        .with_clean_start(true)
        .with_session_expiry_interval(1);
    let client1 = TestClient::connect_with_options(&sut, opts)
        .await
        .expect("first connect failed");
    client1
        .subscribe("sess/expiry", SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    client1.disconnect().await.expect("disconnect failed");

    tokio::time::sleep(Duration::from_secs(5)).await;

    let opts2 = ConnectOptions::new(&client_id)
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let client2 = TestClient::connect_with_options(&sut, opts2)
        .await
        .expect("reconnect failed");
    assert!(
        !client2.session_present(),
        "[MQTT-4.1.0-2] Session must be discarded after expiry interval passes"
    );

    client2.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-2.1.3-1]` Where a flag bit is marked as Reserved, it is reserved
/// for future use and MUST be set to the value listed.
#[conformance_test(
    ids = ["MQTT-2.1.3-1"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn reserved_flags_correct_on_server_packets(sut: SutHandle) {
    let client_id = unique_client_id("rflags");

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    let connack_data = raw
        .read_packet_bytes(TIMEOUT)
        .await
        .expect("Must receive CONNACK");
    assert_eq!(
        connack_data[0], 0x20,
        "[MQTT-2.1.3-1] CONNACK fixed header byte must be 0x20"
    );

    let topic = format!("rflags/{client_id}");
    raw.send_raw(&RawPacketBuilder::subscribe(&topic, 0))
        .await
        .unwrap();
    let suback_data = raw
        .read_packet_bytes(TIMEOUT)
        .await
        .expect("Must receive SUBACK");
    assert_eq!(
        suback_data[0], 0x90,
        "[MQTT-2.1.3-1] SUBACK fixed header byte must be 0x90"
    );

    raw.send_raw(&RawPacketBuilder::publish_qos1(&topic, b"test", 1))
        .await
        .unwrap();
    let puback_data = loop {
        let pkt = raw
            .read_mqtt_packet(TIMEOUT)
            .await
            .expect("Must receive PUBACK");
        if pkt[0] & 0xF0 == 0x30 {
            continue;
        }
        break pkt;
    };
    assert_eq!(
        puback_data[0], 0x40,
        "[MQTT-2.1.3-1] PUBACK fixed header byte must be 0x40"
    );
}

fn build_connect_username_flag_set_no_username() -> Vec<u8> {
    use bytes::{BufMut, BytesMut};

    let mut body = BytesMut::new();
    body.put_u16(4);
    body.put_slice(b"MQTT");
    body.put_u8(5);
    body.put_u8(0x82);
    body.put_u16(60);
    body.put_u8(0);
    put_mqtt_string(&mut body, "malformed-uflag");

    wrap_fixed_header(0x10, &body)
}

fn build_connect_with_username_and_password(
    client_id: &str,
    username: &str,
    password: &str,
) -> Vec<u8> {
    use bytes::{BufMut, BytesMut};

    let mut body = BytesMut::new();
    body.put_u16(4);
    body.put_slice(b"MQTT");
    body.put_u8(5);
    body.put_u8(0xC2);
    body.put_u16(60);
    body.put_u8(0);
    put_mqtt_string(&mut body, client_id);
    put_mqtt_string(&mut body, username);
    let pw_bytes = password.as_bytes();
    body.put_u16(u16::try_from(pw_bytes.len()).unwrap());
    body.put_slice(pw_bytes);

    wrap_fixed_header(0x10, &body)
}
