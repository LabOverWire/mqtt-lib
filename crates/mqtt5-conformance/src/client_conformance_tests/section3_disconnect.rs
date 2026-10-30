use super::common::{
    packets_until_quiet, start, subscribe_deferred_and_ack, wait_for_received, TIMEOUT,
};
use crate::client_conformance_test;
use crate::client_sut::{ClientSutHandle, ConnectSpec, Settle};
use crate::scripted_broker::{publish, publish_first_byte, Packet, DISCONNECT};
use std::time::Duration;

pub const DISCONNECT_REASON_CODES: [u8; 29] = [
    0x00, 0x04, 0x80, 0x81, 0x82, 0x83, 0x87, 0x89, 0x8B, 0x8D, 0x8E, 0x8F, 0x90, 0x93, 0x94, 0x95,
    0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F, 0xA0, 0xA1, 0xA2,
];

/// [MQTT-3.14.2-1] [MQTT-3.14.4-2] A Client that disconnects sends a
/// DISCONNECT Reason Code and then closes the Network Connection.
#[client_conformance_test(ids = ["MQTT-3.14.2-1", "MQTT-3.14.4-2"])]
async fn client_disconnect_uses_disconnect_reason_code_and_closes(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("disc-user"), false, &[]).await;
    s.result.as_ref().unwrap();
    let disconnecting = tokio::spawn(s.client.disconnect());
    let close = s.conn.observe_close(Duration::from_secs(3)).await;
    let _ = tokio::time::timeout(TIMEOUT, disconnecting).await;
    let reason = close
        .disconnect_reason
        .expect("disconnect did not put a DISCONNECT on the wire");
    assert!(
        DISCONNECT_REASON_CODES.contains(&reason),
        "[MQTT-3.14.2-1] DISCONNECT reason code 0x{reason:02X} is not a DISCONNECT Reason Code"
    );
    assert!(
        close.closed,
        "[MQTT-3.14.4-2] the client sent DISCONNECT but left the network connection open for 3s"
    );
}

/// [MQTT-3.14.4-1] A deferred acknowledgement settled just before
/// disconnecting is sent before the DISCONNECT or not at all, never after it.
#[client_conformance_test(ids = ["MQTT-3.14.4-1"], requires = ["client.deferred_ack"])]
async fn no_packet_after_disconnect_while_deferred_ack_settles(sut: ClientSutHandle) {
    for round in 0..10 {
        let spec = ConnectSpec::new(&format!("disc-race-{round}"))
            .resuming(300)
            .with_receive_maximum(16)
            .with_deferred_ack();
        let mut s = start(&sut, spec, false, &[]).await;
        s.result.as_ref().unwrap();
        subscribe_deferred_and_ack(&mut s, "t/race", 1).await;
        s.conn
            .send(&publish(
                publish_first_byte(1, false),
                b"t/race",
                Some(11),
                &[],
                b"x",
            ))
            .await
            .unwrap();
        wait_for_received(&s.client, 1).await;
        assert_eq!(
            s.client.settle_deferred(Settle::Ack),
            1,
            "the message was delivered with a token"
        );
        let disconnecting = tokio::spawn(s.client.disconnect());
        let types: Vec<u8> = packets_until_quiet(&mut s.conn, Duration::from_secs(2))
            .await
            .iter()
            .map(Packet::packet_type)
            .collect();
        let _ = tokio::time::timeout(TIMEOUT, disconnecting).await;
        let position = types
            .iter()
            .position(|t| *t == DISCONNECT)
            .unwrap_or_else(|| panic!("round {round}: disconnect sent no DISCONNECT: {types:?}"));
        assert_eq!(
            position,
            types.len() - 1,
            "[MQTT-3.14.4-1] round {round}: packets sent after DISCONNECT (packet types {types:?})"
        );
    }
}

/// [MQTT-3.14.1-1] [MQTT-3.14.4-2] A DISCONNECT with reserved flags set is
/// malformed: the Client answers with DISCONNECT 0x81 and closes the Network
/// Connection.
#[client_conformance_test(ids = ["MQTT-3.14.1-1", "MQTT-3.14.4-2"])]
async fn disconnect_with_reserved_flags_answered_with_0x81(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("disc-flags"), false, &[]).await;
    s.result.as_ref().unwrap();
    s.conn.send(&[0xE1, 0x00]).await.unwrap();
    let close = s.conn.observe_close(Duration::from_secs(5)).await;
    assert_eq!(
        close.disconnect_reason,
        Some(0x81),
        "[MQTT-3.14.1-1] a DISCONNECT with reserved flags 0x1 was not answered with DISCONNECT 0x81 (closed {})",
        close.closed
    );
    assert!(
        close.closed,
        "[MQTT-3.14.4-2] the client sent DISCONNECT 0x81 but left the network connection open"
    );
}
