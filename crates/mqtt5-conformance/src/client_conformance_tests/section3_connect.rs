use super::common::{
    accept_next, begin, drop_connection, parse_publish, read_connect, received_after, start,
    subscribe_and_ack, SHORT, TIMEOUT,
};
use crate::client_conformance_test;
use crate::client_sut::{AuthSpec, ClientSutHandle, ConnectSpec, PublishSpec, WillSpec};
use crate::packet_parser::ParsedConnect;
use crate::scripted_broker::{
    ack, ack_with_properties, auth, connack, property_string, property_u16, publish,
    subscription_ack, user_property, Next, ScriptedBroker, AUTH, DISCONNECT, PINGREQ,
    PROP_ASSIGNED_CLIENT_IDENTIFIER, PROP_AUTHENTICATION_METHOD, PROP_REASON_STRING,
    PROP_SERVER_KEEP_ALIVE, PUBLISH, PUBREL, SUBSCRIBE,
};
use std::time::Duration;

fn parse_connect(bytes: &[u8]) -> ParsedConnect {
    ParsedConnect::parse(bytes).expect("client sent a malformed CONNECT")
}

fn test_authentication() -> AuthSpec {
    AuthSpec {
        method: "TEST".to_owned(),
        initial_response: Some(b"init".to_vec()),
        challenge_response: b"resp".to_vec(),
        abort_after_challenges: None,
    }
}

/// [MQTT-3.1.2-9] [MQTT-3.1.2-11] [MQTT-3.1.2-13] [MQTT-3.1.2-16]
/// [MQTT-3.1.2-17] [MQTT-3.1.2-18] [MQTT-3.1.2-19] [MQTT-3.1.3-1]
/// [MQTT-2.2.2-1] The CONNECT payload carries exactly the fields its flags
/// declare, in the order the specification gives.
#[client_conformance_test(
    ids = [
        "MQTT-3.1.2-9",
        "MQTT-3.1.2-11",
        "MQTT-3.1.2-13",
        "MQTT-3.1.2-16",
        "MQTT-3.1.2-17",
        "MQTT-3.1.2-18",
        "MQTT-3.1.2-19",
        "MQTT-3.1.3-1",
        "MQTT-2.2.2-1",
    ],
)]
async fn connect_flags_payload_order_and_will(sut: ClientSutHandle) {
    let will = WillSpec {
        topic: "will/t".to_owned(),
        payload: b"bye".to_vec(),
        qos: 1,
    };
    let spec = ConnectSpec::new("cid-1")
        .with_will(will)
        .with_credentials("user", b"pass");
    let s = start(&sut, spec, false, &[]).await;
    let c = ParsedConnect::parse(s.connect.bytes()).expect(
        "[MQTT-3.1.2-9] [MQTT-3.1.2-17] [MQTT-3.1.2-19] [MQTT-3.1.3-1] CONNECT payload does not hold the fields its flags declare",
    );
    assert_eq!(c.flags & 0x01, 0, "reserved CONNECT flag");
    assert_eq!(c.flags & 0x04, 0x04, "will flag");
    let will = c.will.expect("[MQTT-3.1.2-9] will fields present");
    assert_eq!(will.topic, b"will/t", "[MQTT-3.1.2-9] will topic");
    assert_eq!(will.payload, b"bye", "[MQTT-3.1.2-9] will payload");
    assert_eq!(
        c.username.as_deref(),
        Some(&b"user"[..]),
        "[MQTT-3.1.2-17] [MQTT-3.1.3-1] username"
    );
    assert_eq!(
        c.password.as_deref(),
        Some(&b"pass"[..]),
        "[MQTT-3.1.2-19] [MQTT-3.1.3-1] password"
    );
    assert_eq!(c.client_id, b"cid-1", "[MQTT-3.1.3-1] client id first");
    assert_eq!(
        c.trailing_bytes, 0,
        "[MQTT-3.1.3-1] no trailing bytes after declared fields"
    );
    assert_eq!(c.keep_alive, 60);

    let s2 = start(&sut, ConnectSpec::new("cid-2"), false, &[]).await;
    let c2 = parse_connect(s2.connect.bytes());
    assert_eq!(c2.flags & 0x04, 0, "no will flag without a will");
    assert_eq!(
        c2.flags & 0x18,
        0,
        "[MQTT-3.1.2-11] Will QoS must be 0 without will"
    );
    assert_eq!(
        c2.flags & 0x20,
        0,
        "[MQTT-3.1.2-13] Will Retain must be 0 without will"
    );
    assert_eq!(c2.flags & 0x80, 0, "no user name flag without a user name");
    assert_eq!(c2.flags & 0x40, 0, "no password flag without a password");
    assert_eq!(
        c2.trailing_bytes, 0,
        "[MQTT-3.1.2-16] [MQTT-3.1.2-18] no user name or password present"
    );
    assert_eq!(
        c2.properties_length, 0,
        "[MQTT-2.2.2-1] CONNECT without properties has Property Length 0"
    );
}

/// [MQTT-1.5.4-2] The Client must not encode U+0000 in a User Name or a
/// Topic Name.
#[client_conformance_test(ids = ["MQTT-1.5.4-2"])]
async fn null_character_never_encoded(sut: ClientSutHandle) {
    let broker = ScriptedBroker::bind().await.unwrap();
    let client = sut.create(ConnectSpec::new("nul").with_credentials("us\0er", b"p"));
    let connecting = tokio::spawn(client.connect(&broker.url().unwrap()));
    if let Some(mut conn) = broker.accept(TIMEOUT).await {
        let got = conn.next(Duration::from_secs(2)).await;
        let outcome = tokio::time::timeout(TIMEOUT, connecting).await;
        assert!(
            !matches!(got, Next::Packet(ref p) if p.body().windows(5).any(|w| w == b"us\0er")),
            "[MQTT-1.5.4-2] CONNECT User Name carries U+0000 (connect {outcome:?})"
        );
    }

    let mut s = start(&sut, ConnectSpec::new("nul-topic"), false, &[]).await;
    s.result.as_ref().unwrap();
    let outcome = s.client.publish("a\0b", b"x", PublishSpec::default()).await;
    let sent = s
        .conn
        .next_of_type(PUBLISH, Duration::from_millis(500))
        .await;
    assert!(
        sent.is_none(),
        "[MQTT-1.5.4-2] PUBLISH topic with U+0000 sent ({outcome:?})"
    );
}

/// [MQTT-3.1.2-20] An idle Client with Keep Alive 1 s sends a PINGREQ in
/// every Keep Alive period.
#[client_conformance_test(ids = ["MQTT-3.1.2-20"])]
async fn pingreq_sent_when_idle(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("ka1").with_keep_alive(Duration::from_secs(1));
    let mut s = start(&sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    let mut pings = 0;
    for _ in 0..3 {
        match s.conn.next(Duration::from_millis(1100)).await {
            Next::Packet(packet) if packet.packet_type() == PINGREQ => {
                pings += 1;
                s.conn.send(&[0xD0, 0x00]).await.unwrap();
            }
            _ => break,
        }
    }
    assert_eq!(
        pings, 3,
        "[MQTT-3.1.2-20] Keep Alive=1s idle client did not send a PINGREQ within each 1.1s window"
    );
}

/// [MQTT-3.1.2-21] [MQTT-3.2.2-21] The Client uses the Server Keep Alive from
/// CONNACK instead of its own Keep Alive.
#[client_conformance_test(ids = ["MQTT-3.1.2-21", "MQTT-3.2.2-21"])]
async fn server_keep_alive_overrides_client_value(sut: ClientSutHandle) {
    let mut s = start(
        &sut,
        ConnectSpec::new("ska"),
        false,
        &property_u16(PROP_SERVER_KEEP_ALIVE, 1),
    )
    .await;
    s.result.as_ref().unwrap();
    let ping = s
        .conn
        .next_of_type(PINGREQ, Duration::from_millis(1600))
        .await;
    assert!(
        ping.is_some(),
        "[MQTT-3.1.2-21] [MQTT-3.2.2-21] Server Keep Alive=1 but no PINGREQ within 1.6s (client used its own 60s)"
    );
}

/// [MQTT-3.1.2-23] [MQTT-4.4.0-1] [MQTT-3.3.1-1] An unacknowledged `QoS` 1
/// PUBLISH is resent with its original Packet Identifier and DUP 1 when the
/// session resumes.
#[client_conformance_test(
    ids = ["MQTT-3.1.2-23", "MQTT-4.4.0-1", "MQTT-3.3.1-1"],
    requires = ["client.automatic_reconnect"],
)]
async fn unacked_qos1_publish_resent_on_session_resume(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("resend1").reconnecting().resuming(300);
    let mut s = start(&sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    let publishing = tokio::spawn(s.client.publish("r/t", b"x", PublishSpec::qos(1)));
    let original = parse_publish(&s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap());
    let (client, broker) = drop_connection(s).await;
    let (mut conn, _) = accept_next(&broker, true, &[]).await;
    let resent = conn
        .next_of_type(PUBLISH, Duration::from_secs(3))
        .await
        .map(|packet| parse_publish(&packet));
    drop(client);
    publishing.abort();
    assert!(
        resent
            .as_ref()
            .is_some_and(|p| p.packet_id == original.packet_id && p.dup),
        "[MQTT-3.1.2-23] [MQTT-4.4.0-1] [MQTT-3.3.1-1] unacknowledged QoS1 PUBLISH id {:?} not resent (DUP=1, same id) after reconnect with Clean Start=0 and Session Present=1; got {resent:?}",
        original.packet_id
    );
}

/// [MQTT-3.1.2-23] [MQTT-4.4.0-1] An unacknowledged PUBREL is resent with its
/// original Packet Identifier when the session resumes.
#[client_conformance_test(
    ids = ["MQTT-3.1.2-23", "MQTT-4.4.0-1"],
    requires = ["client.automatic_reconnect"],
)]
async fn unacked_pubrel_resent_on_session_resume(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("resend2").reconnecting().resuming(300);
    let mut s = start(&sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    let publishing = tokio::spawn(s.client.publish("r/t", b"x", PublishSpec::qos(2)));
    let original = parse_publish(&s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap());
    let packet_id = original.packet_id.unwrap();
    s.conn.send(&ack(0x50, packet_id)).await.unwrap();
    s.conn
        .next_of_type(PUBREL, TIMEOUT)
        .await
        .expect("no PUBREL");
    let (client, broker) = drop_connection(s).await;
    let (mut conn, _) = accept_next(&broker, true, &[]).await;
    let resent = conn.next_of_type(PUBREL, Duration::from_secs(3)).await;
    drop(client);
    publishing.abort();
    assert!(
        resent
            .as_ref()
            .is_some_and(|r| r.leading_packet_id() == Some(packet_id)),
        "[MQTT-3.1.2-23] [MQTT-4.4.0-1] PUBREL for id {packet_id} not resent after reconnect with Clean Start=0 and Session Present=1"
    );
}

/// [MQTT-3.1.3-2] A Client holding Session State under a server-assigned
/// Client Identifier uses that identifier when it reconnects.
#[client_conformance_test(
    ids = ["MQTT-3.1.3-2"],
    requires = ["client.automatic_reconnect"],
)]
async fn assigned_client_identifier_used_to_resume_session(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("").reconnecting().resuming(300);
    let s = start(
        &sut,
        spec,
        false,
        &property_string(PROP_ASSIGNED_CLIENT_IDENTIFIER, b"srv-assigned-1"),
    )
    .await;
    s.result.as_ref().unwrap();
    let first = parse_connect(s.connect.bytes());
    let (client, broker) = drop_connection(s).await;
    let (_conn, connect) = accept_next(&broker, true, &[]).await;
    drop(client);
    let second = parse_connect(connect.bytes());
    assert_eq!(
        String::from_utf8_lossy(&second.client_id),
        "srv-assigned-1",
        "[MQTT-3.1.3-2] client holding Session State (Clean Start=0, SEI=300) under server-assigned id reconnected with ClientID {:?} (first CONNECT used {:?}), so the state it holds cannot be identified",
        String::from_utf8_lossy(&second.client_id),
        String::from_utf8_lossy(&first.client_id)
    );
}

/// [MQTT-3.1.2-29] [MQTT-4.13.2-1] With Request Problem Information 0, a
/// PUBACK carrying a Reason String is a Protocol Error: the Client sends
/// DISCONNECT 0x82 and closes the Network Connection.
#[client_conformance_test(ids = ["MQTT-3.1.2-29", "MQTT-4.13.2-1"])]
async fn reason_string_on_puback_with_problem_information_0_disconnects(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("rpi-puback").without_problem_information();
    let mut s = start(&sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    let publishing = tokio::spawn(s.client.publish("p/t", b"x", PublishSpec::qos(1)));
    let packet = s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap();
    let packet_id = parse_publish(&packet).packet_id.unwrap();
    s.conn
        .send(&ack_with_properties(
            0x40,
            packet_id,
            0x00,
            &property_string(PROP_REASON_STRING, b"why"),
        ))
        .await
        .unwrap();
    let close = s.conn.observe_close(Duration::from_secs(2)).await;
    let result = tokio::time::timeout(Duration::from_secs(1), publishing).await;
    assert_eq!(
        close.disconnect_reason,
        Some(0x82),
        "[MQTT-3.1.2-29] with Request Problem Information=0 configured, PUBACK carrying a Reason String did not trigger DISCONNECT 0x82 (closed {}, publish result {result:?})",
        close.closed
    );
    assert!(
        close.closed,
        "[MQTT-4.13.2-1] the client sent DISCONNECT 0x82 but left the network connection open"
    );
}

/// [MQTT-3.1.2-29] [MQTT-4.13.2-1] With Request Problem Information 0, a
/// SUBACK carrying a User Property is a Protocol Error: the Client sends
/// DISCONNECT 0x82 and closes the Network Connection.
#[client_conformance_test(ids = ["MQTT-3.1.2-29", "MQTT-4.13.2-1"])]
async fn user_property_on_suback_with_problem_information_0_disconnects(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("rpi-suback").without_problem_information();
    let mut s = start(&sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    let subscribing = tokio::spawn(s.client.subscribe("s/t", 0));
    let subscribe = s.conn.next_of_type(SUBSCRIBE, TIMEOUT).await.unwrap();
    let packet_id = subscribe.leading_packet_id().unwrap();
    s.conn
        .send(&subscription_ack(
            0x90,
            packet_id,
            &user_property(b"k", b"v"),
            &[0x00],
        ))
        .await
        .unwrap();
    let close = s.conn.observe_close(Duration::from_secs(2)).await;
    let result = tokio::time::timeout(Duration::from_secs(1), subscribing).await;
    assert_eq!(
        close.disconnect_reason,
        Some(0x82),
        "[MQTT-3.1.2-29] with Request Problem Information=0 configured, SUBACK carrying a User Property did not trigger DISCONNECT 0x82 (closed {}, subscribe result {result:?})",
        close.closed
    );
    assert!(
        close.closed,
        "[MQTT-4.13.2-1] the client sent DISCONNECT 0x82 but left the network connection open"
    );
}

/// [MQTT-3.1.2-29] [MQTT-4.13.2-1] With Request Problem Information 0, a
/// re-authentication AUTH carrying a Reason String is a Protocol Error: the
/// Client sends DISCONNECT 0x82 and closes the Network Connection.
#[client_conformance_test(
    ids = ["MQTT-3.1.2-29", "MQTT-4.13.2-1"],
    requires = ["client.enhanced_auth"],
)]
async fn reason_string_on_reauth_with_problem_information_0_disconnects(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("rpi-auth")
        .without_problem_information()
        .with_authentication(test_authentication());
    let mut s = start(
        &sut,
        spec,
        false,
        &property_string(PROP_AUTHENTICATION_METHOD, b"TEST"),
    )
    .await;
    s.result.as_ref().unwrap();
    s.client.reauthenticate().await.unwrap();
    let reauth = s
        .conn
        .next_of_type(AUTH, TIMEOUT)
        .await
        .expect("no re-auth AUTH");
    assert_eq!(
        reauth.body().first(),
        Some(&0x19),
        "re-authentication reason code"
    );
    s.conn
        .send(&auth(
            0x00,
            b"TEST",
            &property_string(PROP_REASON_STRING, b"why"),
        ))
        .await
        .unwrap();
    let close = s.conn.observe_close(Duration::from_secs(2)).await;
    assert_eq!(
        close.disconnect_reason,
        Some(0x82),
        "[MQTT-3.1.2-29] with Request Problem Information=0 configured, re-auth AUTH carrying a Reason String did not trigger DISCONNECT 0x82 (closed {})",
        close.closed
    );
    assert!(
        close.closed,
        "[MQTT-4.13.2-1] the client sent DISCONNECT 0x82 but left the network connection open"
    );
}

/// [MQTT-3.1.2-30] A Client that set an Authentication Method sends only AUTH
/// or DISCONNECT before it receives CONNACK.
#[client_conformance_test(
    ids = ["MQTT-3.1.2-30"],
    requires = ["client.enhanced_auth"],
)]
async fn only_auth_before_connack_when_auth_method_set(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("auth-seq").with_authentication(test_authentication());
    let mut pending = begin(&sut, spec).await;
    let connect = parse_connect(read_connect(&mut pending.conn).await.bytes());
    assert_eq!(
        connect.properties.authentication_method.as_deref(),
        Some("TEST"),
        "CONNECT carries the Authentication Method"
    );
    let spam = pending.client.clone();
    let spammer = tokio::spawn(async move {
        let mut calls = tokio::task::JoinSet::new();
        for _ in 0..50 {
            let publish = spam.publish("x", b"y", PublishSpec::default());
            calls.spawn(async move { publish.await.is_ok() });
            let subscribe = spam.subscribe("z", 0);
            calls.spawn(async move { subscribe.await.is_ok() });
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        while calls.join_next().await.is_some() {}
    });
    pending.conn.send(&auth(0x18, b"TEST", &[])).await.unwrap();
    let mut before_connack = Vec::new();
    while let Next::Packet(packet) = pending.conn.next(Duration::from_millis(500)).await {
        before_connack.push(packet.packet_type());
    }
    spammer.abort();
    pending
        .conn
        .send(&connack(
            false,
            &property_string(PROP_AUTHENTICATION_METHOD, b"TEST"),
        ))
        .await
        .unwrap();
    let _ = tokio::time::timeout(TIMEOUT, pending.connecting).await;
    assert!(
        before_connack
            .iter()
            .all(|t| *t == AUTH || *t == DISCONNECT),
        "[MQTT-3.1.2-30] packets other than AUTH/DISCONNECT sent before CONNACK: {before_connack:?}"
    );
    assert!(
        before_connack.contains(&AUTH),
        "client did not answer AUTH challenge"
    );
}

/// [MQTT-3.1.2-24] A server that sends a packet larger than the Client's
/// Maximum Packet Size breaks the rule; the Client does not deliver it and
/// ends the connection (DISCONNECT 0x95 or a close).
#[client_conformance_test(ids = ["MQTT-3.1.2-24"])]
async fn inbound_packet_above_client_maximum_packet_size(sut: ClientSutHandle) {
    let mut s = start(
        &sut,
        ConnectSpec::new("conn-maxpkt").with_maximum_packet_size(128),
        false,
        &[],
    )
    .await;
    s.result.as_ref().unwrap();
    subscribe_and_ack(&mut s, "t/big", 0).await;
    s.conn
        .send(&publish(0x30, b"t/big", None, &[], &[b'x'; 1000]))
        .await
        .unwrap();
    let delivered = received_after(&s.client, SHORT).await.len();
    let close = s.conn.observe_close(Duration::from_secs(3)).await;
    assert!(
        delivered == 0 && close.disconnect_reason.map_or(close.closed, |rc| rc == 0x95),
        "[MQTT-3.1.2-24] packet larger than client Maximum Packet Size 128 was not refused; delivered={delivered} closed={} DISCONNECT reason {:?}",
        close.closed,
        close.disconnect_reason
    );
}
