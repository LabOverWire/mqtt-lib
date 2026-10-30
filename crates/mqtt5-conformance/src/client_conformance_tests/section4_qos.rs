use super::common::{
    packets_until_quiet, parse_publish, publishes_until_quiet, queued_flush, received_after, start,
    subscribe_and_ack, SHORT, TIMEOUT,
};
use crate::client_conformance_test;
use crate::client_sut::{ClientError, ClientSutHandle, ConnectSpec, PublishSpec};
use crate::packet_parser::ParsedPublish;
use crate::scripted_broker::{
    ack, ack_with_properties, publish, publish_first_byte, BrokerConnection, Next, Packet, PUBACK,
    PUBCOMP, PUBLISH, PUBREC, PUBREL,
};
use std::sync::Arc;
use std::time::Duration;

/// [MQTT-4.3.3-10] [MQTT-4.3.3-12] A duplicate `QoS` 2 PUBLISH that arrives
/// before PUBREL is acknowledged again with PUBREC and not delivered twice;
/// after PUBCOMP the same Packet Identifier starts a new Application
/// Message.
#[client_conformance_test(ids = ["MQTT-4.3.3-10", "MQTT-4.3.3-12"])]
async fn duplicate_qos2_before_pubrel_delivered_once_then_id_reusable(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("qos-dup2"), false, &[]).await;
    s.result.as_ref().unwrap();
    subscribe_and_ack(&mut s, "t/q2", 2).await;

    s.conn
        .send(&publish(
            publish_first_byte(2, false),
            b"t/q2",
            Some(5),
            &[],
            b"first",
        ))
        .await
        .unwrap();
    assert_eq!(
        s.conn
            .next_of_type(PUBREC, TIMEOUT)
            .await
            .and_then(|p| p.leading_packet_id()),
        Some(5)
    );
    s.conn
        .send(&publish(
            publish_first_byte(2, true),
            b"t/q2",
            Some(5),
            &[],
            b"first",
        ))
        .await
        .unwrap();
    assert_eq!(
        s.conn
            .next_of_type(PUBREC, TIMEOUT)
            .await
            .and_then(|p| p.leading_packet_id()),
        Some(5),
        "[MQTT-4.3.3-10] duplicate before PUBREL must be re-acknowledged with PUBREC"
    );
    assert_eq!(
        received_after(&s.client, SHORT).await.len(),
        1,
        "[MQTT-4.3.3-10] duplicate QoS2 PUBLISH before PUBREL delivered more than once"
    );

    s.conn.send(&ack(0x62, 5)).await.unwrap();
    assert_eq!(
        s.conn
            .next_of_type(PUBCOMP, TIMEOUT)
            .await
            .and_then(|p| p.leading_packet_id()),
        Some(5)
    );

    s.conn
        .send(&publish(
            publish_first_byte(2, false),
            b"t/q2",
            Some(5),
            &[],
            b"second",
        ))
        .await
        .unwrap();
    assert_eq!(
        s.conn
            .next_of_type(PUBREC, TIMEOUT)
            .await
            .and_then(|p| p.leading_packet_id()),
        Some(5)
    );
    let received = received_after(&s.client, SHORT).await;
    assert_eq!(
        received.len(),
        2,
        "[MQTT-4.3.3-12] PUBLISH reusing a completed QoS2 id must be a new message"
    );
    assert_eq!(received[1].payload, b"second");
}

/// [MQTT-4.3.2-5] After PUBACK, a `QoS` 1 PUBLISH with the same Packet
/// Identifier is a new Application Message, whatever its DUP flag.
#[client_conformance_test(ids = ["MQTT-4.3.2-5"])]
async fn qos1_id_reuse_after_puback_is_new_message(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("qos-reuse1"), false, &[]).await;
    s.result.as_ref().unwrap();
    subscribe_and_ack(&mut s, "t/q1", 1).await;
    for (dup, payload) in [(false, &b"a"[..]), (true, &b"b"[..])] {
        s.conn
            .send(&publish(
                publish_first_byte(1, dup),
                b"t/q1",
                Some(9),
                &[],
                payload,
            ))
            .await
            .unwrap();
        assert_eq!(
            s.conn
                .next_of_type(PUBACK, TIMEOUT)
                .await
                .and_then(|p| p.leading_packet_id()),
            Some(9)
        );
    }
    assert_eq!(
        received_after(&s.client, SHORT).await.len(),
        2,
        "[MQTT-4.3.2-5] QoS1 PUBLISH reusing an acknowledged id must be treated as new"
    );
}

/// [MQTT-4.3.2-2] A `QoS` 1 message queued while offline is sent for the first
/// time with DUP 0.
#[client_conformance_test(
    ids = ["MQTT-4.3.2-2"],
    requires = ["client.automatic_reconnect", "client.offline_queue"],
)]
async fn queued_message_first_transmission_has_dup_zero(sut: ClientSutHandle) {
    let publishes = queued_flush(&sut, "qos-queued-dup").await;
    let first = parse_publish(
        publishes
            .first()
            .expect("queued messages were never sent after reconnect"),
    );
    assert!(
        !first.dup,
        "[MQTT-4.3.2-2] queued QoS1 message sent for the first time with DUP=1 (packet id {:?})",
        first.packet_id
    );
}

async fn next_publish(conn: &mut BrokerConnection, what: &str) -> ParsedPublish {
    let packet = conn
        .next_of_type(PUBLISH, TIMEOUT)
        .await
        .unwrap_or_else(|| panic!("no {what} PUBLISH"));
    parse_publish(&packet)
}

/// [MQTT-4.3.1-1] [MQTT-4.3.2-2] [MQTT-4.3.3-2] [MQTT-4.3.3-4] [MQTT-4.3.3-6]
/// A first transmission at each `QoS` has DUP 0, the PUBREL carries the
/// PUBLISH Packet Identifier, and the PUBLISH is not resent once the PUBREL
/// was sent.
#[client_conformance_test(
    ids = ["MQTT-4.3.1-1", "MQTT-4.3.2-2", "MQTT-4.3.3-2", "MQTT-4.3.3-4", "MQTT-4.3.3-6"],
)]
async fn first_transmissions_have_dup_zero_and_pubrel_ends_resends(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("qos-first"), false, &[]).await;
    s.result.as_ref().unwrap();
    let client = Arc::clone(&s.client);
    let publishing = tokio::spawn(async move {
        for qos in 0..=2u8 {
            client
                .publish("t/first", &[qos], PublishSpec::qos(qos))
                .await?;
        }
        Ok::<(), ClientError>(())
    });
    let q0 = next_publish(&mut s.conn, "QoS 0").await;
    assert!(
        q0.qos == 0 && !q0.dup,
        "[MQTT-4.3.1-1] QoS 0 PUBLISH sent with QoS {} DUP {}",
        q0.qos,
        q0.dup
    );
    let q1 = next_publish(&mut s.conn, "QoS 1").await;
    assert!(
        q1.qos == 1 && !q1.dup,
        "[MQTT-4.3.2-2] first QoS 1 transmission sent with QoS {} DUP {}",
        q1.qos,
        q1.dup
    );
    s.conn
        .send(&ack(0x40, q1.packet_id.unwrap()))
        .await
        .unwrap();
    let q2 = next_publish(&mut s.conn, "QoS 2").await;
    assert!(
        q2.qos == 2 && !q2.dup,
        "[MQTT-4.3.3-2] first QoS 2 transmission sent with QoS {} DUP {}",
        q2.qos,
        q2.dup
    );
    let id = q2.packet_id.unwrap();
    s.conn.send(&ack(0x50, id)).await.unwrap();
    let pubrel = s
        .conn
        .next_of_type(PUBREL, TIMEOUT)
        .await
        .expect("no PUBREL after PUBREC");
    assert_eq!(
        pubrel.leading_packet_id(),
        Some(id),
        "[MQTT-4.3.3-4] PUBREL packet id differs from the PUBLISH"
    );
    let resent = publishes_until_quiet(&mut s.conn, SHORT).await;
    assert!(
        resent.is_empty(),
        "[MQTT-4.3.3-6] {} PUBLISH packets sent after the PUBREL",
        resent.len()
    );
    s.conn.send(&ack(0x70, id)).await.unwrap();
    tokio::time::timeout(TIMEOUT, publishing)
        .await
        .expect("publishes did not complete")
        .expect("publish task panicked")
        .expect("publish failed");
}

/// [MQTT-4.4.0-2] A PUBREC with a Reason Code of 0x80 or greater ends the
/// exchange: no PUBREL and no resend of the PUBLISH.
#[client_conformance_test(ids = ["MQTT-4.4.0-2"])]
async fn error_pubrec_gets_no_pubrel_or_resend(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("qos-pubrec-err"), false, &[]).await;
    s.result.as_ref().unwrap();
    let publishing = tokio::spawn(s.client.publish("t/err", b"x", PublishSpec::qos(2)));
    let packet = s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap();
    let id = parse_publish(&packet).packet_id.unwrap();
    s.conn
        .send(&ack_with_properties(0x50, id, 0x80, &[]))
        .await
        .unwrap();
    let after: Vec<u8> = packets_until_quiet(&mut s.conn, Duration::from_secs(1))
        .await
        .iter()
        .map(Packet::packet_type)
        .collect();
    let _ = tokio::time::timeout(TIMEOUT, publishing).await;
    assert!(
        !after.contains(&PUBREL) && !after.contains(&PUBLISH),
        "[MQTT-4.4.0-2] the client continued the exchange after PUBREC 0x80 (packet types {after:?})"
    );
}

/// [MQTT-4.5.0-2] The Client acknowledges a PUBLISH it holds no subscription
/// for.
#[client_conformance_test(ids = ["MQTT-4.5.0-2"])]
async fn publish_without_subscription_is_acknowledged(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("qos-nosub"), false, &[]).await;
    s.result.as_ref().unwrap();
    for (qos, id, ack_type) in [(1u8, 5u16, PUBACK), (2, 6, PUBREC)] {
        s.conn
            .send(&publish(
                publish_first_byte(qos, false),
                b"unsubscribed",
                Some(id),
                &[],
                b"x",
            ))
            .await
            .unwrap();
        assert_eq!(
            s.conn
                .next_of_type(ack_type, TIMEOUT)
                .await
                .and_then(|p| p.leading_packet_id()),
            Some(id),
            "[MQTT-4.5.0-2] QoS {qos} PUBLISH without a matching subscription was not acknowledged"
        );
    }
}

/// [MQTT-4.3.2-1] A Packet Identifier whose PUBREL is still unacknowledged is
/// not given to a new `QoS` 1 message, even after the identifiers wrap.
#[client_conformance_test(ids = ["MQTT-4.3.2-1"])]
async fn packet_id_not_reused_while_pubrel_outstanding(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("qos-pid-rel"), false, &[]).await;
    s.result.as_ref().unwrap();
    let held = tokio::spawn(s.client.publish("t/held", b"h", PublishSpec::qos(2)));
    let packet = s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap();
    let held_id = parse_publish(&packet).packet_id.unwrap();
    s.conn.send(&ack(0x50, held_id)).await.unwrap();
    s.conn
        .next_of_type(PUBREL, TIMEOUT)
        .await
        .expect("no PUBREL after PUBREC");

    let (tx, mut rx) = tokio::sync::mpsc::channel::<u16>(16);
    let mut conn = s.conn;
    let acker = tokio::spawn(async move {
        loop {
            match conn.next_answering_pings(Duration::from_secs(60)).await {
                Next::Packet(packet) if packet.packet_type() == PUBLISH => {
                    let id = parse_publish(&packet).packet_id.unwrap();
                    let _ = tx.send(id).await;
                    let _ = conn.send(&ack(0x40, id)).await;
                }
                Next::Packet(_) => {}
                Next::Malformed | Next::Closed | Next::Silent => return,
            }
        }
    });

    let mut reused = None;
    for sent in 0..65535u32 {
        let _ =
            tokio::time::timeout(TIMEOUT, s.client.publish("t/x", b"x", PublishSpec::qos(1))).await;
        match tokio::time::timeout(TIMEOUT, rx.recv()).await {
            Ok(Some(id)) if id == held_id => {
                reused = Some(id);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {
                panic!("client sent no PUBLISH within {TIMEOUT:?} after {sent} publishes")
            }
        }
    }
    acker.abort();
    held.abort();
    assert!(
        reused.is_none(),
        "[MQTT-4.3.2-1] packet id {held_id} given to a new QoS 1 message while its QoS 2 PUBREL was still unacknowledged"
    );
}
