use super::common::{
    accept_next, begin, drop_connection, finish, packets_until_quiet, parse_publish,
    queued_publish_then_reconnect, read_connect, reconnect_with, start, subscribe_and_ack,
    subscribe_deferred_and_ack, wait_connected, wait_for_received, TIMEOUT,
};
use crate::client_conformance_test;
use crate::client_sut::{ClientSutHandle, ClientUnderTest, ConnectSpec, PublishSpec, Settle};
use crate::scripted_broker::{
    ack, property_u16, property_u32, property_u8, publish, publish_first_byte, BrokerConnection,
    Packet, PROP_MAXIMUM_PACKET_SIZE, PROP_MAXIMUM_QOS, PROP_RETAIN_AVAILABLE,
    PROP_TOPIC_ALIAS_MAXIMUM, PUBACK, PUBLISH, PUBREC, PUBREL, SUBSCRIBE, UNSUBSCRIBE,
};
use std::sync::Arc;
use std::time::Duration;

async fn assert_no_alias_sent(
    client: &Arc<dyn ClientUnderTest>,
    conn: &mut BrokerConnection,
    alias: u16,
    statement: &str,
) {
    let spec = PublishSpec {
        topic_alias: Some(alias),
        ..PublishSpec::default()
    };
    let result = client.publish("alias/topic", b"x", spec).await;
    if let Some(packet) = conn.next_of_type(PUBLISH, Duration::from_secs(1)).await {
        let sent = parse_publish(&packet);
        assert!(
            sent.properties.topic_alias.is_none(),
            "[{statement}] client sent Topic Alias {alias} in PUBLISH ({:?}); publish returned {result:?}",
            sent.properties
        );
    }
}

/// [MQTT-3.2.2-1] A CONNACK with a reserved Connect Acknowledge Flags bit set
/// must not be accepted.
#[client_conformance_test(ids = ["MQTT-3.2.2-1"])]
async fn connack_reserved_bits_rejected(sut: ClientSutHandle) {
    let mut pending = begin(&sut, ConnectSpec::new("connack-flags")).await;
    read_connect(&mut pending.conn).await;
    pending
        .conn
        .send(&[0x20, 0x03, 0x02, 0x00, 0x00])
        .await
        .unwrap();
    let result = finish(pending.connecting).await;
    let close = pending.conn.observe_close(Duration::from_secs(2)).await;
    assert!(
        result.is_err() && close.closed,
        "[MQTT-3.2.2-1] CONNACK with reserved flag bit 1 set accepted: {result:?}, closed {}",
        close.closed
    );
}

/// [MQTT-3.2.2-4] A Client with no Session State that receives Session Present
/// 1 must close the Network Connection (Clean Start 1).
#[client_conformance_test(ids = ["MQTT-3.2.2-4"])]
async fn fresh_client_clean_start_1_session_present_1_must_close(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("sp-clean"), true, &[]).await;
    let close = s.conn.observe_close(Duration::from_secs(2)).await;
    assert!(
        s.result.is_err() && close.closed,
        "[MQTT-3.2.2-4] fresh client (Clean Start=1, no session state) accepted CONNACK Session Present=1: connect result {:?}, network closed={}",
        s.result,
        close.closed
    );
}

/// [MQTT-3.2.2-4] A Client with no Session State that receives Session Present
/// 1 must close the Network Connection (Clean Start 0).
#[client_conformance_test(ids = ["MQTT-3.2.2-4"])]
async fn fresh_client_clean_start_0_session_present_1_must_close(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("sp-fresh").resuming(300), true, &[]).await;
    let close = s.conn.observe_close(Duration::from_secs(2)).await;
    assert!(
        s.result.is_err() && close.closed,
        "[MQTT-3.2.2-4] brand-new client with no session state accepted CONNACK Session Present=1: connect result {:?}, network closed={}",
        s.result,
        close.closed
    );
}

/// [MQTT-3.2.2-11] After CONNACK Maximum `QoS` 0 the Client must not send a
/// PUBLISH above `QoS` 0.
#[client_conformance_test(ids = ["MQTT-3.2.2-11"])]
async fn maximum_qos_limits_live_publish(sut: ClientSutHandle) {
    let mut s = start(
        &sut,
        ConnectSpec::new("mq0"),
        false,
        &property_u8(PROP_MAXIMUM_QOS, 0),
    )
    .await;
    s.result.as_ref().unwrap();
    let publishing = tokio::spawn(s.client.publish("q/t", b"x", PublishSpec::qos(2)));
    if let Some(packet) = s.conn.next_of_type(PUBLISH, Duration::from_secs(1)).await {
        assert_eq!(
            parse_publish(&packet).qos,
            0,
            "[MQTT-3.2.2-11] PUBLISH QoS exceeds server Maximum QoS 0"
        );
    }
    publishing.abort();
}

/// [MQTT-3.2.2-11] A message queued while offline is sent at no more than the
/// Maximum `QoS` of the connection it is sent on.
#[client_conformance_test(
    ids = ["MQTT-3.2.2-11"],
    requires = ["client.automatic_reconnect", "client.offline_queue"],
)]
async fn maximum_qos_applies_to_messages_queued_while_offline(sut: ClientSutHandle) {
    let sent = queued_publish_then_reconnect(
        &sut,
        "mq-queued",
        &property_u8(PROP_MAXIMUM_QOS, 1),
        PublishSpec::qos(2),
    )
    .await
    .expect("queued message was never sent");
    assert!(
        sent.qos <= 1,
        "[MQTT-3.2.2-11] message queued offline was flushed as QoS {} after CONNACK Maximum QoS=1",
        sent.qos
    );
}

/// [MQTT-3.2.2-11] A Maximum `QoS` on the second connection's CONNACK applies
/// to publishes made after the reconnect.
#[client_conformance_test(
    ids = ["MQTT-3.2.2-11"],
    requires = ["client.automatic_reconnect"],
)]
async fn maximum_qos_newly_imposed_on_reconnect(sut: ClientSutHandle) {
    let (client, _broker, mut conn) =
        reconnect_with(&sut, "new-mq", &[], &property_u8(PROP_MAXIMUM_QOS, 0)).await;
    let publishing = tokio::spawn(client.publish("q/t", b"x", PublishSpec::qos(1)));
    if let Some(packet) = conn.next_of_type(PUBLISH, Duration::from_secs(1)).await {
        assert_eq!(
            parse_publish(&packet).qos,
            0,
            "[MQTT-3.2.2-11] after conn 2 CONNACK Maximum QoS=0 the client still sent QoS>0"
        );
    }
    publishing.abort();
}

/// [MQTT-3.2.2-14] After CONNACK Retain Available 0 the Client must not send
/// a PUBLISH with RETAIN 1.
#[client_conformance_test(ids = ["MQTT-3.2.2-14"])]
async fn retain_available_0_client_must_not_send_retain(sut: ClientSutHandle) {
    let mut s = start(
        &sut,
        ConnectSpec::new("ra0"),
        false,
        &property_u8(PROP_RETAIN_AVAILABLE, 0),
    )
    .await;
    s.result.as_ref().unwrap();
    let spec = PublishSpec {
        retain: true,
        ..PublishSpec::default()
    };
    let result = s.client.publish("r/t", b"x", spec).await;
    if let Some(packet) = s.conn.next_of_type(PUBLISH, Duration::from_secs(1)).await {
        assert!(
            !parse_publish(&packet).retain,
            "[MQTT-3.2.2-14] client sent PUBLISH with RETAIN=1 after CONNACK Retain Available=0 (publish returned {result:?})"
        );
    }
}

/// [MQTT-3.2.2-14] A message queued while offline is not sent with RETAIN 1
/// on a connection whose CONNACK has Retain Available 0.
#[client_conformance_test(
    ids = ["MQTT-3.2.2-14"],
    requires = ["client.automatic_reconnect", "client.offline_queue"],
)]
async fn retain_available_applies_to_messages_queued_while_offline(sut: ClientSutHandle) {
    let spec = PublishSpec {
        qos: 1,
        retain: true,
        ..PublishSpec::default()
    };
    let sent = queued_publish_then_reconnect(
        &sut,
        "ra-queued",
        &property_u8(PROP_RETAIN_AVAILABLE, 0),
        spec,
    )
    .await;
    if let Some(sent) = sent {
        assert!(
            !sent.retain,
            "[MQTT-3.2.2-14] message queued offline was flushed with RETAIN=1 after CONNACK Retain Available=0"
        );
    }
}

/// [MQTT-3.2.2-15] A Maximum Packet Size on the second connection's CONNACK
/// applies to publishes made after the reconnect.
#[client_conformance_test(
    ids = ["MQTT-3.2.2-15"],
    requires = ["client.automatic_reconnect"],
)]
async fn maximum_packet_size_newly_imposed_on_reconnect(sut: ClientSutHandle) {
    let (client, _broker, mut conn) = reconnect_with(
        &sut,
        "new-mps",
        &[],
        &property_u32(PROP_MAXIMUM_PACKET_SIZE, 64),
    )
    .await;
    let result = client
        .publish("big/t", &[b'x'; 500], PublishSpec::default())
        .await;
    let sent = conn.next_of_type(PUBLISH, Duration::from_secs(1)).await;
    assert!(
        sent.as_ref().is_none_or(|p| p.wire_len() <= 64),
        "[MQTT-3.2.2-15] client sent {}-byte PUBLISH after conn 2 CONNACK Maximum Packet Size=64 (publish returned {result:?})",
        sent.map_or(0, |p| p.wire_len())
    );
}

async fn assert_automatic_ack_fits(
    sut: &ClientSutHandle,
    first_byte: u8,
    ack_type: u8,
    name: &str,
) {
    let mut s = start(
        sut,
        ConnectSpec::new(name),
        false,
        &property_u32(PROP_MAXIMUM_PACKET_SIZE, 3),
    )
    .await;
    s.result.as_ref().unwrap();
    s.conn
        .send(&publish(first_byte, b"a", Some(7), &[], b"x"))
        .await
        .unwrap();
    let ack = s.conn.next_of_type(ack_type, Duration::from_secs(1)).await;
    assert!(
        ack.as_ref().is_none_or(|a| a.wire_len() <= 3),
        "[MQTT-3.2.2-15] automatic {name} of {} bytes sent despite server Maximum Packet Size=3",
        ack.map_or(0, |a| a.wire_len())
    );
}

/// [MQTT-3.2.2-15] The PUBACK the Client sends for a `QoS` 1 PUBLISH must fit
/// the server's Maximum Packet Size.
#[client_conformance_test(ids = ["MQTT-3.2.2-15"])]
async fn automatic_puback_respects_tiny_maximum_packet_size(sut: ClientSutHandle) {
    assert_automatic_ack_fits(&sut, 0x32, PUBACK, "PUBACK").await;
}

/// [MQTT-3.2.2-15] The PUBREC the Client sends for a `QoS` 2 PUBLISH must fit
/// the server's Maximum Packet Size.
#[client_conformance_test(ids = ["MQTT-3.2.2-15"])]
async fn automatic_pubrec_respects_tiny_maximum_packet_size(sut: ClientSutHandle) {
    assert_automatic_ack_fits(&sut, 0x34, PUBREC, "PUBREC").await;
}

async fn assert_request_fits(sut: &ClientSutHandle, unsubscribe: bool) {
    let (name, packet_type) = if unsubscribe {
        ("UNSUBSCRIBE", UNSUBSCRIBE)
    } else {
        ("SUBSCRIBE", SUBSCRIBE)
    };
    let mut s = start(
        sut,
        ConnectSpec::new(name),
        false,
        &property_u32(PROP_MAXIMUM_PACKET_SIZE, 64),
    )
    .await;
    s.result.as_ref().unwrap();
    let filter = format!("f/{}", "x".repeat(100));
    let request = if unsubscribe {
        s.client.unsubscribe(&filter)
    } else {
        s.client.subscribe(&filter, 0)
    };
    let requesting = tokio::spawn(request);
    let sent = s
        .conn
        .next_of_type(packet_type, Duration::from_secs(1))
        .await;
    requesting.abort();
    assert!(
        sent.as_ref().is_none_or(|p| p.wire_len() <= 64),
        "[MQTT-3.2.2-15] client sent {}-byte {name} despite server Maximum Packet Size=64",
        sent.map_or(0, |p| p.wire_len())
    );
}

/// [MQTT-3.2.2-15] A SUBSCRIBE larger than the server's Maximum Packet Size
/// must not be sent.
#[client_conformance_test(ids = ["MQTT-3.2.2-15"])]
async fn oversized_subscribe_not_sent(sut: ClientSutHandle) {
    assert_request_fits(&sut, false).await;
}

/// [MQTT-3.2.2-15] An UNSUBSCRIBE larger than the server's Maximum Packet
/// Size must not be sent.
#[client_conformance_test(ids = ["MQTT-3.2.2-15"])]
async fn oversized_unsubscribe_not_sent(sut: ClientSutHandle) {
    assert_request_fits(&sut, true).await;
}

/// [MQTT-3.2.2-17] The Client must not send a Topic Alias greater than the
/// server's Topic Alias Maximum.
#[client_conformance_test(ids = ["MQTT-3.2.2-17"])]
async fn topic_alias_above_server_maximum_not_sent(sut: ClientSutHandle) {
    let mut s = start(
        &sut,
        ConnectSpec::new("ta-max"),
        false,
        &property_u16(PROP_TOPIC_ALIAS_MAXIMUM, 2),
    )
    .await;
    s.result.as_ref().unwrap();
    assert_no_alias_sent(&s.client, &mut s.conn, 3, "MQTT-3.2.2-17").await;
}

/// [MQTT-3.2.2-18] With no Topic Alias Maximum in CONNACK the Client must not
/// send a Topic Alias.
#[client_conformance_test(ids = ["MQTT-3.2.2-18"])]
async fn topic_alias_absent_client_must_not_send_alias(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("ta-absent"), false, &[]).await;
    s.result.as_ref().unwrap();
    assert_no_alias_sent(&s.client, &mut s.conn, 1, "MQTT-3.2.2-18").await;
}

/// [MQTT-3.2.2-18] A Topic Alias Maximum from an earlier connection does not
/// carry over to one whose CONNACK has none.
#[client_conformance_test(
    ids = ["MQTT-3.2.2-18"],
    requires = ["client.automatic_reconnect"],
)]
async fn topic_alias_maximum_not_stale_after_reconnect(sut: ClientSutHandle) {
    let (client, _broker, mut conn) = reconnect_with(
        &sut,
        "stale-ta",
        &property_u16(PROP_TOPIC_ALIAS_MAXIMUM, 10),
        &[],
    )
    .await;
    assert_no_alias_sent(&client, &mut conn, 1, "MQTT-3.2.2-18").await;
}

/// [MQTT-3.2.2-5] A Client that holds Session State and gets Session Present
/// 0 discards it, so acknowledgements left over from the old session are not
/// sent on the new connection.
#[client_conformance_test(
    ids = ["MQTT-3.2.2-5"],
    requires = ["client.automatic_reconnect", "client.deferred_ack"],
)]
async fn queued_acks_discarded_when_session_not_present(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("sp0-acks")
        .resuming(300)
        .with_receive_maximum(4)
        .with_deferred_ack()
        .reconnecting_after(Duration::from_millis(100), Duration::from_millis(200));
    let mut s = start(&sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    subscribe_deferred_and_ack(&mut s, "t/deferred", 1).await;
    subscribe_and_ack(&mut s, "t/auto", 1).await;

    s.conn
        .send(&publish(
            publish_first_byte(1, false),
            b"t/deferred",
            Some(1),
            &[],
            b"held",
        ))
        .await
        .unwrap();
    s.conn
        .send(&publish(
            publish_first_byte(1, false),
            b"t/auto",
            Some(2),
            &[],
            b"auto",
        ))
        .await
        .unwrap();
    let before = s
        .conn
        .next_of_type(PUBACK, Duration::from_millis(400))
        .await;
    assert!(
        before.is_none(),
        "setup: the PUBACK for id 2 waits behind the unresolved token for id 1"
    );
    let (client, broker) = drop_connection(s).await;

    let (mut conn, _) = accept_next(&broker, false, &[]).await;
    wait_connected(&client, true).await;
    assert_eq!(
        client.settle_deferred(Settle::Ack),
        1,
        "setup: one delivery holds a token"
    );
    let puback = conn.next_of_type(PUBACK, Duration::from_millis(800)).await;
    assert!(
        puback.is_none(),
        "[MQTT-3.2.2-5] an acknowledgement from the discarded session was sent on a Session Present=0 connection: packet id {:?}",
        puback.and_then(|p| p.leading_packet_id())
    );
}

/// [MQTT-3.2.2-5] A Client that holds Session State and gets Session Present
/// 0 discards it: no outbound PUBLISH or PUBREL of the old session is resent,
/// and an inbound `QoS` 2 Packet Identifier still awaiting PUBREL starts a new
/// Application Message.
#[client_conformance_test(ids = ["MQTT-3.2.2-5"], requires = ["client.automatic_reconnect"])]
async fn session_state_discarded_when_session_not_present(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("sp0-state").reconnecting().resuming(300);
    let mut s = start(&sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    subscribe_and_ack(&mut s, "t/in", 2).await;
    let publishing = tokio::spawn(s.client.publish("t/out", b"q2", PublishSpec::qos(2)));
    let out_id = parse_publish(&s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap())
        .packet_id
        .unwrap();
    s.conn.send(&ack(0x50, out_id)).await.unwrap();
    s.conn
        .next_of_type(PUBREL, TIMEOUT)
        .await
        .expect("no PUBREL after PUBREC");
    s.conn
        .send(&publish(
            publish_first_byte(2, false),
            b"t/in",
            Some(9),
            &[],
            b"old",
        ))
        .await
        .unwrap();
    s.conn
        .next_of_type(PUBREC, TIMEOUT)
        .await
        .expect("no PUBREC for the inbound QoS 2 PUBLISH");
    assert_eq!(
        wait_for_received(&s.client, 1).await.len(),
        1,
        "the first message was delivered"
    );
    let (client, broker) = drop_connection(s).await;

    let (mut conn, _) = accept_next(&broker, false, &[]).await;
    wait_connected(&client, true).await;
    let resent: Vec<u8> = packets_until_quiet(&mut conn, Duration::from_secs(2))
        .await
        .iter()
        .map(Packet::packet_type)
        .filter(|t| *t == PUBLISH || *t == PUBREL)
        .collect();
    assert!(
        resent.is_empty(),
        "[MQTT-3.2.2-5] packets of the discarded session resent after Session Present 0 (packet types {resent:?})"
    );
    conn.send(&publish(
        publish_first_byte(2, false),
        b"t/in",
        Some(9),
        &[],
        b"new",
    ))
    .await
    .unwrap();
    conn.next_of_type(PUBREC, TIMEOUT)
        .await
        .expect("no PUBREC for the new QoS 2 PUBLISH");
    let received = wait_for_received(&client, 2).await;
    drop(client);
    publishing.abort();
    assert_eq!(
        received.len(),
        2,
        "[MQTT-3.2.2-5] inbound QoS 2 state kept after Session Present 0: the new PUBLISH with id 9 was dropped as a duplicate"
    );
}
