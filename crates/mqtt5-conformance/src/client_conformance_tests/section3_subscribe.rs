use super::common::{parse_publish, start, SHORT, TIMEOUT};
use crate::client_conformance_test;
use crate::client_sut::{ClientSutHandle, ConnectSpec, PublishSpec};
use crate::packet_parser::decode_variable_int;
use crate::scripted_broker::{
    ack, subscription_ack, Packet, PUBLISH, PUBREL, SUBSCRIBE, UNSUBSCRIBE,
};

const PUBREL_REASON_CODES: [u8; 2] = [0x00, 0x92];

fn payload_after_properties(packet: &Packet) -> &[u8] {
    let body = packet.body();
    decode_variable_int(body, 2)
        .and_then(|(len, start)| body.get(start + len as usize..))
        .unwrap_or_default()
}

/// [MQTT-3.8.1-1] [MQTT-3.8.3-2] [MQTT-3.3.1-2] [MQTT-4.3.3-2] [MQTT-3.6.1-1]
/// [MQTT-3.6.2-1] [MQTT-3.10.1-1] [MQTT-3.10.3-2] SUBSCRIBE, UNSUBSCRIBE and
/// PUBREL carry their listed fixed header flags, SUBSCRIBE and UNSUBSCRIBE
/// carry at least one filter, a `QoS` 0 PUBLISH and the first transmission of
/// a `QoS` 2 PUBLISH have DUP 0, and PUBREL uses a PUBREL Reason Code.
#[client_conformance_test(
    ids = [
        "MQTT-3.8.1-1",
        "MQTT-3.8.3-2",
        "MQTT-3.3.1-2",
        "MQTT-4.3.3-2",
        "MQTT-3.6.1-1",
        "MQTT-3.6.2-1",
        "MQTT-3.10.1-1",
        "MQTT-3.10.3-2",
    ],
)]
async fn subscribe_unsubscribe_and_pubrel_wire_format(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("sub-flags"), false, &[]).await;
    s.result.as_ref().unwrap();

    let subscribing = tokio::spawn(s.client.subscribe("t/flags", 0));
    let subscribe = s.conn.next_of_type(SUBSCRIBE, TIMEOUT).await.unwrap();
    assert_eq!(
        subscribe.bytes()[0],
        0x82,
        "[MQTT-3.8.1-1] SUBSCRIBE fixed header flags"
    );
    assert!(
        !payload_after_properties(&subscribe).is_empty(),
        "[MQTT-3.8.3-2] SUBSCRIBE must carry at least one filter"
    );
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
        .unwrap();

    s.client
        .publish("t/q0", b"x", PublishSpec::default())
        .await
        .unwrap();
    let qos0 = parse_publish(&s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap());
    assert!(!qos0.dup, "[MQTT-3.3.1-2] QoS0 PUBLISH must have DUP=0");

    let publishing = tokio::spawn(s.client.publish("t/q2", b"x", PublishSpec::qos(2)));
    let qos2 = parse_publish(&s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap());
    assert!(
        !qos2.dup,
        "[MQTT-4.3.3-2] first QoS2 transmission must have DUP=0"
    );
    let packet_id = qos2.packet_id.unwrap();
    s.conn.send(&ack(0x50, packet_id)).await.unwrap();
    let pubrel = s.conn.next_of_type(PUBREL, TIMEOUT).await.unwrap();
    assert_eq!(
        pubrel.bytes()[0],
        0x62,
        "[MQTT-3.6.1-1] PUBREL fixed header flags"
    );
    assert!(
        PUBREL_REASON_CODES.contains(&pubrel.ack_reason_code()),
        "[MQTT-3.6.2-1] PUBREL reason code 0x{:02X}",
        pubrel.ack_reason_code()
    );
    s.conn.send(&ack(0x70, packet_id)).await.unwrap();
    tokio::time::timeout(TIMEOUT, publishing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let unsubscribing = tokio::spawn(s.client.unsubscribe("t/flags"));
    let unsubscribe = s.conn.next_of_type(UNSUBSCRIBE, TIMEOUT).await.unwrap();
    assert_eq!(
        unsubscribe.bytes()[0],
        0xA2,
        "[MQTT-3.10.1-1] UNSUBSCRIBE fixed header flags"
    );
    assert!(
        !payload_after_properties(&unsubscribe).is_empty(),
        "[MQTT-3.10.3-2] UNSUBSCRIBE must carry at least one filter"
    );
    s.conn
        .send(&subscription_ack(
            0xB0,
            unsubscribe.leading_packet_id().unwrap(),
            &[],
            &[0x00],
        ))
        .await
        .unwrap();
    tokio::time::timeout(TIMEOUT, unsubscribing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// [MQTT-3.8.4-2] A SUBACK whose Packet Identifier is not the SUBSCRIBE's does
/// not complete that subscribe; the matching SUBACK does.
#[client_conformance_test(ids = ["MQTT-3.8.4-2"])]
async fn suback_with_foreign_packet_id_is_not_accepted(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("sub-foreign"), false, &[]).await;
    s.result.as_ref().unwrap();
    let subscribing = tokio::spawn(s.client.subscribe("t/s", 0));
    let packet_id = s
        .conn
        .next_of_type(SUBSCRIBE, TIMEOUT)
        .await
        .unwrap()
        .leading_packet_id()
        .unwrap();
    s.conn
        .send(&subscription_ack(
            0x90,
            packet_id.wrapping_add(100),
            &[],
            &[0x00],
        ))
        .await
        .unwrap();
    tokio::time::sleep(SHORT).await;
    assert!(
        !subscribing.is_finished(),
        "[MQTT-3.8.4-2] subscribe completed on a SUBACK carrying a different Packet Identifier"
    );
    s.conn
        .send(&subscription_ack(0x90, packet_id, &[], &[0x00]))
        .await
        .unwrap();
    tokio::time::timeout(TIMEOUT, subscribing)
        .await
        .expect("[MQTT-3.8.4-2] matching SUBACK completes subscribe")
        .unwrap()
        .unwrap();
}
