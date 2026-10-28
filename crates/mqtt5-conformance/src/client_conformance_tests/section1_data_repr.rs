use super::common::{inject_after_subscribe, start, TIMEOUT};
use crate::client_conformance_test;
use crate::client_sut::{ClientSutHandle, ConnectSpec, PublishSpec};
use crate::scripted_broker::{mqtt_string, publish, variable_int, PROP_USER_PROPERTY, PUBLISH};

pub(super) fn surrogate_topic_publish() -> Vec<u8> {
    publish(0x30, &[b'a', 0xED, 0xA0, 0x80], None, &[], b"x")
}

async fn assert_malformed_rejected(
    sut: &ClientSutHandle,
    bytes: &[u8],
    client_id: &str,
    what: &str,
) {
    let outcome = inject_after_subscribe(sut, client_id, bytes).await;
    assert!(
        outcome.delivered == 0 && !outcome.still_connected,
        "malformed {what} was accepted (delivered {} times, client still connected={})",
        outcome.delivered,
        outcome.still_connected
    );
}

/// [MQTT-1.5.4-1] A PUBLISH whose Topic Name encodes a UTF-16 surrogate is a
/// Malformed Packet: the Client does not deliver it and drops the connection.
#[client_conformance_test(ids = ["MQTT-1.5.4-1"])]
async fn surrogate_in_inbound_topic_is_malformed(sut: ClientSutHandle) {
    assert_malformed_rejected(
        &sut,
        &surrogate_topic_publish(),
        "surrogate-topic",
        "[MQTT-1.5.4-1] PUBLISH topic with UTF-16 surrogate",
    )
    .await;
}

/// [MQTT-1.5.4-2] A PUBLISH whose Topic Name contains U+0000 is a Malformed
/// Packet: the Client does not deliver it and drops the connection.
#[client_conformance_test(ids = ["MQTT-1.5.4-2"])]
async fn null_in_inbound_topic_is_malformed(sut: ClientSutHandle) {
    assert_malformed_rejected(
        &sut,
        &publish(0x30, b"a\0b", None, &[], b"x"),
        "null-topic",
        "[MQTT-1.5.4-2] PUBLISH topic containing U+0000",
    )
    .await;
}

/// [MQTT-1.5.7-1] A User Property whose value is not valid UTF-8 makes the
/// PUBLISH a Malformed Packet: the Client does not deliver it and drops the
/// connection.
#[client_conformance_test(ids = ["MQTT-1.5.7-1"])]
async fn invalid_utf8_user_property_pair_is_malformed(sut: ClientSutHandle) {
    let mut prop = vec![PROP_USER_PROPERTY];
    prop.extend(mqtt_string(b"k"));
    prop.extend(mqtt_string(&[0xFF, 0xFE]));
    assert_malformed_rejected(
        &sut,
        &publish(0x30, b"a", None, &prop, b"x"),
        "bad-user-property",
        "[MQTT-1.5.7-1] PUBLISH with invalid UTF-8 User Property value",
    )
    .await;
}

/// [MQTT-1.5.5-1] The Remaining Length the Client sends uses the minimum
/// number of bytes.
#[client_conformance_test(ids = ["MQTT-1.5.5-1"])]
async fn remaining_length_minimal_encoding(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("varint"), false, &[]).await;
    s.result.as_ref().unwrap();
    for size in [100usize, 125, 126, 200, 16_380, 20_000] {
        s.client
            .publish("v", &vec![0u8; size], PublishSpec::default())
            .await
            .unwrap();
        let packet = s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap();
        assert_eq!(
            packet.remaining_length_bytes(),
            variable_int(packet.body().len()),
            "[MQTT-1.5.5-1] non-minimal Remaining Length encoding"
        );
    }
}
