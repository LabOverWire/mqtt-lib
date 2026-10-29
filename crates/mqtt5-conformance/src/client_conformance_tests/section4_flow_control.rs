use super::common::{
    accept_next, drop_connection, parse_publish, publishes_until_quiet, queued_flush,
    received_after, start, subscribe_and_ack, wait_connected, SHORT, TIMEOUT,
};
use crate::client_conformance_test;
use crate::client_sut::{ClientSutHandle, ConnectSpec, PublishOutcome, PublishSpec};
use crate::packet_parser::ParsedConnect;
use crate::scripted_broker::{
    ack, property_u16, publish, publish_first_byte, subscription_ack, Next, PROP_RECEIVE_MAXIMUM,
    PUBLISH, PUBREC, SUBSCRIBE,
};
use std::time::Duration;

/// [MQTT-3.3.4-9] A server that sends more unacknowledged `QoS` 2 PUBLISH
/// packets than the Client's Receive Maximum breaks the rule; the Client
/// answers with DISCONNECT 0x93 (Receive Maximum exceeded).
#[client_conformance_test(ids = ["MQTT-3.3.4-9"])]
async fn server_exceeding_client_receive_maximum_gets_disconnect_0x93(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("flow-rm-in").with_receive_maximum(1);
    let mut s = start(&sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    subscribe_and_ack(&mut s, "t/rm", 2).await;
    s.conn
        .send(&publish(
            publish_first_byte(2, false),
            b"t/rm",
            Some(1),
            &[],
            b"1",
        ))
        .await
        .unwrap();
    assert_eq!(
        s.conn
            .next_of_type(PUBREC, TIMEOUT)
            .await
            .and_then(|p| p.leading_packet_id()),
        Some(1)
    );
    s.conn
        .send(&publish(
            publish_first_byte(2, false),
            b"t/rm",
            Some(2),
            &[],
            b"2",
        ))
        .await
        .unwrap();
    let close = s.conn.observe_close(Duration::from_secs(3)).await;
    let delivered = received_after(&s.client, SHORT).await.len();
    assert_eq!(
        close.disconnect_reason,
        Some(0x93),
        "[MQTT-3.3.4-9] a second unacknowledged QoS2 PUBLISH with client Receive Maximum 1 must draw DISCONNECT 0x93 (delivered {delivered})"
    );
}

/// [MQTT-3.3.4-7] [MQTT-4.9.0-2] After resuming a session on a connection
/// whose CONNACK lowers Receive Maximum to 1, the Client has at most one
/// unacknowledged `QoS` 1 PUBLISH on the wire, and new publishes still
/// complete once it is acknowledged.
#[client_conformance_test(
    ids = ["MQTT-3.3.4-7", "MQTT-4.9.0-2"],
    requires = ["client.automatic_reconnect"],
)]
async fn session_resume_with_smaller_receive_maximum(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("flow-resume-rm")
        .resuming(300)
        .reconnecting_after(Duration::from_millis(100), Duration::from_millis(200));
    let mut s = start(&sut, spec, false, &property_u16(PROP_RECEIVE_MAXIMUM, 2)).await;
    s.result.as_ref().unwrap();
    let old: Vec<_> = ["t/old/1", "t/old/2"]
        .into_iter()
        .map(|topic| tokio::spawn(s.client.publish(topic, b"old", PublishSpec::qos(1))))
        .collect();
    let old_ids = publishes_until_quiet(&mut s.conn, SHORT).await.len();
    assert_eq!(old_ids, 2, "both QoS1 publishes reach the wire on RM=2");
    let (client, broker) = drop_connection(s).await;

    let (mut conn, connect) =
        accept_next(&broker, true, &property_u16(PROP_RECEIVE_MAXIMUM, 1)).await;
    let reconnect = ParsedConnect::parse(connect.bytes()).expect("client sent a malformed CONNECT");
    assert_eq!(
        reconnect.flags & 0x02,
        0,
        "client reconnected with Clean Start 1 instead of resuming"
    );
    wait_connected(&client, true).await;
    let new: Vec<_> = ["t/new/1", "t/new/2"]
        .into_iter()
        .map(|topic| tokio::spawn(client.publish(topic, b"new", PublishSpec::qos(1))))
        .collect();

    let mut pending: Vec<u16> = publishes_until_quiet(&mut conn, Duration::from_millis(1500))
        .await
        .iter()
        .filter_map(|packet| parse_publish(packet).packet_id)
        .collect();
    assert!(
        pending.len() <= 1,
        "[MQTT-3.3.4-7] client had {} unacknowledged QoS1 PUBLISH on the wire with server Receive Maximum 1: {pending:?}",
        pending.len()
    );

    let mut acked = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while acked < old_ids + 2 && tokio::time::Instant::now() < deadline {
        for id in pending.drain(..) {
            conn.send(&ack(0x40, id)).await.unwrap();
            acked += 1;
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match conn.next_answering_pings(left).await {
            Next::Packet(packet) if packet.packet_type() == PUBLISH => {
                pending.extend(parse_publish(&packet).packet_id);
            }
            Next::Closed => panic!("client dropped the resumed connection"),
            Next::Packet(_) | Next::Malformed | Next::Silent => {}
        }
    }
    for handle in new {
        let result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("[MQTT-4.9.0-2] new publish deadlocked after session resume")
            .expect("publish task panicked");
        assert!(
            result.is_ok(),
            "new publish after resume failed: {result:?}"
        );
    }
    for handle in old {
        handle.abort();
    }
}

/// [MQTT-4.9.0-1] A new connection starts with a non-zero send quota, even
/// when a publish on the previous connection was never acknowledged.
#[client_conformance_test(ids = ["MQTT-4.9.0-1"], requires = ["client.automatic_reconnect"])]
async fn send_quota_reinitialized_on_new_connection(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("flow-quota")
        .reconnecting_after(Duration::from_millis(100), Duration::from_millis(200));
    let receive_maximum_1 = property_u16(PROP_RECEIVE_MAXIMUM, 1);
    let s = start(&sut, spec, false, &receive_maximum_1).await;
    s.result.as_ref().unwrap();
    let never_acked = tokio::time::timeout(
        Duration::from_secs(30),
        s.client
            .publish("t/quota", b"never-acked", PublishSpec::qos(1)),
    )
    .await
    .expect("publish never returned");
    assert!(
        matches!(never_acked, Ok(PublishOutcome::Pending)),
        "an unacknowledged publish returns as pending: {never_acked:?}"
    );
    let (client, broker) = drop_connection(s).await;

    let (mut conn, _) = accept_next(&broker, false, &receive_maximum_1).await;
    wait_connected(&client, true).await;
    let fresh = tokio::spawn(client.publish("t/quota", b"fresh", PublishSpec::qos(1)));
    let sent = conn
        .next_of_type(PUBLISH, Duration::from_secs(2))
        .await
        .map(|packet| parse_publish(&packet).payload);
    fresh.abort();
    assert_eq!(
        sent.as_deref(),
        Some(&b"fresh"[..]),
        "[MQTT-4.9.0-1] new connection (Session Present 0, Receive Maximum 1) did not send the new publish, so it started with a send quota of 0"
    );
}

/// [MQTT-3.3.4-7] Messages queued while offline are flushed without
/// exceeding the Receive Maximum of the new connection.
#[client_conformance_test(
    ids = ["MQTT-3.3.4-7"],
    requires = ["client.automatic_reconnect", "client.offline_queue"],
)]
async fn queued_messages_flushed_within_server_receive_maximum(sut: ClientSutHandle) {
    let publishes = queued_flush(&sut, "flow-queued-rm").await;
    assert!(
        !publishes.is_empty(),
        "queued messages were never sent after reconnect"
    );
    assert!(
        publishes.len() <= 1,
        "[MQTT-3.3.4-7] client flushed {} unacknowledged queued QoS1 PUBLISH packets with server Receive Maximum 1",
        publishes.len()
    );
}

/// [MQTT-3.3.4-8] With the send quota used up, the Client still sends
/// SUBSCRIBE and DISCONNECT without delay.
#[client_conformance_test(ids = ["MQTT-3.3.4-8"])]
async fn disconnect_not_delayed_by_exhausted_send_quota(sut: ClientSutHandle) {
    let mut s = start(
        &sut,
        ConnectSpec::new("flow-nodelay"),
        false,
        &property_u16(PROP_RECEIVE_MAXIMUM, 1),
    )
    .await;
    s.result.as_ref().unwrap();
    let blocked: Vec<_> = (0..2u8)
        .map(|i| tokio::spawn(s.client.publish("t/hold", &[i], PublishSpec::qos(1))))
        .collect();
    s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap();

    let subscribing = tokio::spawn(s.client.subscribe("t/other", 0));
    let subscribe = s
        .conn
        .next_of_type(SUBSCRIBE, Duration::from_secs(2))
        .await
        .expect("[MQTT-3.3.4-8] SUBSCRIBE was delayed while the send quota was exhausted");
    s.conn
        .send(&subscription_ack(
            0x90,
            subscribe.leading_packet_id().unwrap(),
            &[],
            &[0x00],
        ))
        .await
        .unwrap();
    tokio::time::timeout(TIMEOUT, subscribing)
        .await
        .unwrap()
        .unwrap()
        .expect("subscribe while quota is exhausted");

    let disconnecting = tokio::spawn(s.client.disconnect());
    let close = s.conn.observe_close(Duration::from_secs(2)).await;
    disconnecting.abort();
    for handle in blocked {
        handle.abort();
    }
    assert!(
        close.disconnect_reason.is_some(),
        "[MQTT-3.3.4-8] DISCONNECT was delayed while the send quota was exhausted (nothing within 2s)"
    );
}
