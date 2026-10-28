use super::common::{parse_publish, start, TIMEOUT};
use crate::client_conformance_test;
use crate::client_sut::{ClientSutHandle, ConnectSpec, PublishOutcome, PublishSpec};
use crate::scripted_broker::{
    ack, publish, subscription_ack, Next, DISCONNECT, PINGREQ, PUBACK, PUBCOMP, PUBLISH, PUBREC,
    PUBREL, SUBSCRIBE, UNSUBSCRIBE,
};
use std::time::Duration;

/// [MQTT-2.1.3-1] A PUBACK with reserved fixed header flags set is malformed
/// and must not be taken as an acknowledgement.
#[client_conformance_test(ids = ["MQTT-2.1.3-1"])]
async fn reserved_flags_on_inbound_puback_is_malformed(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("flags-puback"), false, &[]).await;
    s.result.as_ref().unwrap();
    let publishing = tokio::spawn(s.client.publish("p/t", b"x", PublishSpec::qos(1)));
    let packet_id = parse_publish(&s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap())
        .packet_id
        .unwrap();
    s.conn.send(&ack(0x42, packet_id)).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), publishing)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !matches!(result, Ok(PublishOutcome::Completed)) && !s.client.is_connected().await,
        "[MQTT-2.1.3-1] PUBACK with reserved flag bits 0x2 accepted as a valid acknowledgement ({result:?})"
    );
}

/// [MQTT-2.1.3-1] A SUBACK with reserved fixed header flags set is malformed
/// and must not be taken as an acknowledgement.
#[client_conformance_test(ids = ["MQTT-2.1.3-1"])]
async fn reserved_flags_on_inbound_suback_is_malformed(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("flags-suback"), false, &[]).await;
    s.result.as_ref().unwrap();
    let subscribing = tokio::spawn(s.client.subscribe("s/t"));
    let subscribe = s.conn.next_of_type(SUBSCRIBE, TIMEOUT).await.unwrap();
    let packet_id = subscribe.leading_packet_id().unwrap();
    s.conn
        .send(&subscription_ack(0x92, packet_id, &[], &[0x00]))
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), subscribing)
        .await
        .unwrap()
        .unwrap();
    let still_connected = s.client.is_connected().await;
    assert!(
        result.is_err() && !still_connected,
        "[MQTT-2.1.3-1] SUBACK with reserved flag bits 0x2 accepted as a valid acknowledgement ({result:?}, still connected={still_connected})"
    );
}

/// [MQTT-2.2.1-2] [MQTT-2.2.2-1] [MQTT-3.3.1-2] A `QoS` 0 PUBLISH has no Packet
/// Identifier, a Property Length of 0 when it has no properties, and DUP 0.
#[client_conformance_test(ids = ["MQTT-2.2.1-2", "MQTT-2.2.2-1", "MQTT-3.3.1-2"])]
async fn qos0_publish_has_no_packet_id_and_zero_property_length(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("qos0"), false, &[]).await;
    s.result.as_ref().unwrap();
    s.client
        .publish("t", b"p", PublishSpec::default())
        .await
        .unwrap();
    let packet = s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap();
    let body = packet.body();
    assert!(
        body.len() <= 5,
        "[MQTT-2.2.1-2] QoS0 PUBLISH carries bytes for a packet id: body {body:?}"
    );
    assert_eq!(
        body,
        [0x00, 0x01, b't', 0x00, b'p'],
        "[MQTT-2.2.2-1] QoS0 PUBLISH without properties must be topic, Property Length 0, payload"
    );
    assert_eq!(
        packet.flags() & 0x08,
        0,
        "[MQTT-3.3.1-2] QoS0 PUBLISH with DUP 1"
    );
}

/// [MQTT-2.2.1-5] [MQTT-2.1.3-1] The PUBACK, PUBREC and PUBCOMP the Client
/// sends carry the inbound PUBLISH's Packet Identifier and the listed fixed
/// header flags.
#[client_conformance_test(ids = ["MQTT-2.2.1-5", "MQTT-2.1.3-1"])]
async fn acks_echo_inbound_packet_id(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("ack-ids"), false, &[]).await;
    s.result.as_ref().unwrap();
    s.conn
        .send(&publish(0x32, b"a", Some(0x1234), &[], b"x"))
        .await
        .unwrap();
    let puback = s.conn.next_of_type(PUBACK, TIMEOUT).await.unwrap();
    assert_eq!(puback.flags(), 0, "[MQTT-2.1.3-1] PUBACK flags");
    assert_eq!(
        puback.leading_packet_id(),
        Some(0x1234),
        "[MQTT-2.2.1-5] PUBACK id"
    );
    s.conn
        .send(&publish(0x34, b"a", Some(0x4321), &[], b"x"))
        .await
        .unwrap();
    let pubrec = s.conn.next_of_type(PUBREC, TIMEOUT).await.unwrap();
    assert_eq!(pubrec.flags(), 0, "[MQTT-2.1.3-1] PUBREC flags");
    assert_eq!(
        pubrec.leading_packet_id(),
        Some(0x4321),
        "[MQTT-2.2.1-5] PUBREC id"
    );
    s.conn.send(&ack(0x62, 0x4321)).await.unwrap();
    let pubcomp = s.conn.next_of_type(PUBCOMP, TIMEOUT).await.unwrap();
    assert_eq!(pubcomp.flags(), 0, "[MQTT-2.1.3-1] PUBCOMP flags");
    assert_eq!(
        pubcomp.leading_packet_id(),
        Some(0x4321),
        "[MQTT-2.2.1-5] PUBCOMP id"
    );
}

/// [MQTT-2.1.3-1] [MQTT-2.2.1-3] [MQTT-2.2.1-5] SUBSCRIBE, UNSUBSCRIBE,
/// PUBREL and DISCONNECT carry the listed fixed header flags, every new
/// Packet Identifier is non-zero, and PUBREL echoes its PUBLISH's identifier.
#[client_conformance_test(ids = ["MQTT-2.1.3-1", "MQTT-2.2.1-3", "MQTT-2.2.1-5"])]
async fn outbound_reserved_flags_and_nonzero_packet_ids(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("out-flags"), false, &[]).await;
    s.result.as_ref().unwrap();
    let subscribing = tokio::spawn(s.client.subscribe("s/t"));
    let subscribe = s.conn.next_of_type(SUBSCRIBE, TIMEOUT).await.unwrap();
    assert_eq!(subscribe.flags(), 0x02, "[MQTT-2.1.3-1] SUBSCRIBE flags");
    let packet_id = subscribe.leading_packet_id().unwrap();
    assert_ne!(packet_id, 0, "[MQTT-2.2.1-3] zero SUBSCRIBE id");
    s.conn
        .send(&subscription_ack(0x90, packet_id, &[], &[0x00]))
        .await
        .unwrap();
    tokio::time::timeout(TIMEOUT, subscribing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let unsubscribing = tokio::spawn(s.client.unsubscribe("s/t"));
    let unsubscribe = s.conn.next_of_type(UNSUBSCRIBE, TIMEOUT).await.unwrap();
    assert_eq!(
        unsubscribe.flags(),
        0x02,
        "[MQTT-2.1.3-1] UNSUBSCRIBE flags"
    );
    let packet_id = unsubscribe.leading_packet_id().unwrap();
    assert_ne!(packet_id, 0, "[MQTT-2.2.1-3] zero UNSUBSCRIBE id");
    s.conn
        .send(&subscription_ack(0xB0, packet_id, &[], &[0x00]))
        .await
        .unwrap();
    tokio::time::timeout(TIMEOUT, unsubscribing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let publishing = tokio::spawn(s.client.publish("q2", b"x", PublishSpec::qos(2)));
    let packet = s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap();
    let packet_id = parse_publish(&packet).packet_id.unwrap();
    assert_ne!(packet_id, 0, "[MQTT-2.2.1-3] zero PUBLISH id");
    s.conn.send(&ack(0x50, packet_id)).await.unwrap();
    let pubrel = s.conn.next_of_type(PUBREL, TIMEOUT).await.unwrap();
    assert_eq!(pubrel.flags(), 0x02, "[MQTT-2.1.3-1] PUBREL flags");
    assert_eq!(
        pubrel.leading_packet_id(),
        Some(packet_id),
        "[MQTT-2.2.1-5] PUBREL id"
    );
    s.conn.send(&ack(0x70, packet_id)).await.unwrap();
    tokio::time::timeout(TIMEOUT, publishing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    s.client.disconnect().await.unwrap();
    let disconnect = s.conn.next_of_type(DISCONNECT, TIMEOUT).await.unwrap();
    assert_eq!(disconnect.flags(), 0, "[MQTT-2.1.3-1] DISCONNECT flags");
}

/// [MQTT-2.2.1-3] A Packet Identifier still held by an unacknowledged `QoS` 1
/// PUBLISH is not given to a new PUBLISH, even after the identifiers wrap.
#[client_conformance_test(ids = ["MQTT-2.2.1-3"])]
async fn packet_id_not_reused_while_in_flight_after_wrap(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("pid-wrap"), false, &[]).await;
    s.result.as_ref().unwrap();
    let held = tokio::spawn(s.client.publish("w/held", b"h", PublishSpec::qos(1)));
    let held_id = parse_publish(&s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap())
        .packet_id
        .unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<u16>();
    let mut conn = s.conn;
    let acker = tokio::spawn(async move {
        loop {
            match conn.next(Duration::from_secs(60)).await {
                Next::Packet(packet) if packet.packet_type() == PUBLISH => {
                    let id = parse_publish(&packet).packet_id.unwrap();
                    let _ = tx.send(id);
                    if id != held_id {
                        let _ = conn.send(&ack(0x40, id)).await;
                    }
                }
                Next::Packet(packet) if packet.packet_type() == PINGREQ => {
                    let _ = conn.send(&[0xD0, 0x00]).await;
                }
                Next::Packet(_) => {}
                Next::Malformed | Next::Closed | Next::Silent => return,
            }
        }
    });

    let mut reused = None;
    for _ in 0..65535u32 {
        let _ =
            tokio::time::timeout(TIMEOUT, s.client.publish("w/t", b"x", PublishSpec::qos(1))).await;
        if let Some(id) = rx.recv().await {
            if id == held_id {
                reused = Some(id);
                break;
            }
        }
    }
    acker.abort();
    held.abort();
    assert!(
        reused.is_none(),
        "[MQTT-2.2.1-3] packet id {held_id} reassigned to a new PUBLISH while the original QoS1 PUBLISH with that id was still unacknowledged"
    );
}
