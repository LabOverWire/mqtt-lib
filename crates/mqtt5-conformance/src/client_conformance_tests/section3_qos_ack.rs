use super::common::{
    received_after, start, subscribe_and_ack, subscribe_deferred_and_ack, wait_for_received, SHORT,
    TIMEOUT,
};
use crate::client_conformance_test;
use crate::client_sut::{ClientSutHandle, ConnectSpec, Settle};
use crate::scripted_broker::{ack, publish, publish_first_byte, PUBACK, PUBCOMP, PUBREC};

const PUBLISH_ACK_REASON_CODES: [u8; 9] = [0x00, 0x10, 0x80, 0x83, 0x87, 0x90, 0x91, 0x97, 0x99];
const PUBCOMP_REASON_CODES: [u8; 2] = [0x00, 0x92];

/// [MQTT-3.3.4-1] [MQTT-3.4.2-1] [MQTT-3.5.2-1] [MQTT-3.7.2-1] [MQTT-4.3.3-11]
/// The Client answers a `QoS` 1 PUBLISH with PUBACK, a `QoS` 2 PUBLISH with
/// PUBREC and a PUBREL with PUBCOMP, each with a valid Reason Code and the
/// Packet Identifier it acknowledges.
#[client_conformance_test(
    ids = ["MQTT-3.3.4-1", "MQTT-3.4.2-1", "MQTT-3.5.2-1", "MQTT-3.7.2-1", "MQTT-4.3.3-11"],
)]
async fn client_acks_inbound_by_qos(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("ack-in"), false, &[]).await;
    s.result.as_ref().unwrap();
    subscribe_and_ack(&mut s, "t/in", 2).await;

    s.conn
        .send(&publish(
            publish_first_byte(1, false),
            b"t/in",
            Some(11),
            &[],
            b"q1",
        ))
        .await
        .unwrap();
    let puback = s
        .conn
        .next_of_type(PUBACK, TIMEOUT)
        .await
        .expect("[MQTT-3.3.4-1] no PUBACK for a QoS 1 PUBLISH");
    assert_eq!(puback.leading_packet_id(), Some(11), "PUBACK id");
    assert!(
        PUBLISH_ACK_REASON_CODES.contains(&puback.ack_reason_code()),
        "[MQTT-3.4.2-1] PUBACK reason code 0x{:02X}",
        puback.ack_reason_code()
    );

    s.conn
        .send(&publish(
            publish_first_byte(2, false),
            b"t/in",
            Some(12),
            &[],
            b"q2",
        ))
        .await
        .unwrap();
    let pubrec = s
        .conn
        .next_of_type(PUBREC, TIMEOUT)
        .await
        .expect("[MQTT-3.3.4-1] no PUBREC for a QoS 2 PUBLISH");
    assert_eq!(pubrec.leading_packet_id(), Some(12), "PUBREC id");
    assert!(
        PUBLISH_ACK_REASON_CODES.contains(&pubrec.ack_reason_code()),
        "[MQTT-3.5.2-1] PUBREC reason code 0x{:02X}",
        pubrec.ack_reason_code()
    );
    s.conn.send(&ack(0x62, 12)).await.unwrap();
    let pubcomp = s
        .conn
        .next_of_type(PUBCOMP, TIMEOUT)
        .await
        .expect("no PUBCOMP for a PUBREL");
    assert_eq!(
        pubcomp.leading_packet_id(),
        Some(12),
        "[MQTT-4.3.3-11] PUBCOMP id"
    );
    assert!(
        PUBCOMP_REASON_CODES.contains(&pubcomp.ack_reason_code()),
        "[MQTT-3.7.2-1] PUBCOMP reason code 0x{:02X}",
        pubcomp.ack_reason_code()
    );

    assert_eq!(received_after(&s.client, SHORT).await.len(), 2);
}

/// [MQTT-4.3.3-11] [MQTT-3.7.2-1] A PUBREL for a Packet Identifier the
/// Client holds no state for is still answered with a PUBCOMP carrying that
/// identifier and a PUBCOMP Reason Code.
#[client_conformance_test(ids = ["MQTT-4.3.3-11", "MQTT-3.7.2-1"])]
async fn pubrel_for_unknown_id_is_completed(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("ack-pubrel"), false, &[]).await;
    s.result.as_ref().unwrap();
    s.conn.send(&ack(0x62, 77)).await.unwrap();
    let pubcomp = s
        .conn
        .next_of_type(PUBCOMP, TIMEOUT)
        .await
        .expect("[MQTT-4.3.3-11] no PUBCOMP for a PUBREL");
    assert_eq!(
        pubcomp.leading_packet_id(),
        Some(77),
        "[MQTT-4.3.3-11] PUBCOMP id"
    );
    assert!(
        PUBCOMP_REASON_CODES.contains(&pubcomp.ack_reason_code()),
        "[MQTT-3.7.2-1] PUBCOMP reason code 0x{:02X}",
        pubcomp.ack_reason_code()
    );
}

async fn deferred_reject_reason(sut: &ClientSutHandle, qos: u8, ack_type: u8) -> u8 {
    let spec = ConnectSpec::new("ack-reject")
        .resuming(300)
        .with_receive_maximum(10)
        .with_deferred_ack();
    let mut s = start(sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    subscribe_deferred_and_ack(&mut s, "t/rej", qos).await;
    s.conn
        .send(&publish(
            publish_first_byte(qos, false),
            b"t/rej",
            Some(21),
            &[],
            b"x",
        ))
        .await
        .unwrap();
    wait_for_received(&s.client, 1).await;
    assert_eq!(
        s.client.settle_deferred(Settle::Reject(0x89)),
        1,
        "the message was delivered with a token"
    );
    s.conn
        .next_of_type(ack_type, TIMEOUT)
        .await
        .expect("no acknowledgement after the reject")
        .ack_reason_code()
}

/// [MQTT-3.4.2-1] A deferred rejection with Reason Code 0x89 (Server busy),
/// which is not a PUBACK Reason Code, still puts a PUBACK Reason Code on the
/// wire.
#[client_conformance_test(ids = ["MQTT-3.4.2-1"], requires = ["client.deferred_ack"])]
async fn deferred_reject_puback_reason_code_must_be_valid(sut: ClientSutHandle) {
    let reason = deferred_reject_reason(&sut, 1, PUBACK).await;
    assert!(
        PUBLISH_ACK_REASON_CODES.contains(&reason),
        "[MQTT-3.4.2-1] a deferred reject put PUBACK reason code 0x{reason:02X} on the wire, which is not a PUBACK Reason Code"
    );
}

/// [MQTT-3.5.2-1] A deferred rejection with Reason Code 0x89 (Server busy),
/// which is not a PUBREC Reason Code, still puts a PUBREC Reason Code on the
/// wire.
#[client_conformance_test(ids = ["MQTT-3.5.2-1"], requires = ["client.deferred_ack"])]
async fn deferred_reject_pubrec_reason_code_must_be_valid(sut: ClientSutHandle) {
    let reason = deferred_reject_reason(&sut, 2, PUBREC).await;
    assert!(
        PUBLISH_ACK_REASON_CODES.contains(&reason),
        "[MQTT-3.5.2-1] a deferred reject put PUBREC reason code 0x{reason:02X} on the wire, which is not a PUBREC Reason Code"
    );
}
