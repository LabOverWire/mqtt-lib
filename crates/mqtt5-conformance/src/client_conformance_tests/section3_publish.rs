use super::common::{parse_publish, received_after, start, subscribe_and_ack, SHORT};
use crate::client_conformance_test;
use crate::client_sut::{ClientError, ClientSutHandle, ConnectSpec, PublishOutcome, PublishSpec};
use crate::packet_parser::ParsedConnect;
use crate::scripted_broker::{
    property_u16, publish, publish_first_byte, Packet, PROP_TOPIC_ALIAS, PROP_TOPIC_ALIAS_MAXIMUM,
    PUBLISH,
};
use std::time::Duration;

async fn published(
    sut: &ClientSutHandle,
    client_id: &str,
    connack_props: &[u8],
    topic: &str,
    spec: PublishSpec,
) -> (Result<PublishOutcome, ClientError>, Option<Packet>) {
    let mut s = start(sut, ConnectSpec::new(client_id), false, connack_props).await;
    s.result.as_ref().unwrap();
    let result = s.client.publish(topic, b"x", spec).await;
    (result, s.conn.next_of_type(PUBLISH, SHORT).await)
}

/// [MQTT-3.3.2-2] The Topic Name of a PUBLISH the Client sends contains no
/// wildcard characters.
#[client_conformance_test(ids = ["MQTT-3.3.2-2"])]
async fn publish_topic_must_not_contain_wildcards(sut: ClientSutHandle) {
    let (result, sent) = published(&sut, "pub-wild", &[], "a/+/b", PublishSpec::default()).await;
    if let Some(packet) = sent {
        panic!(
            "[MQTT-3.3.2-2] client sent PUBLISH with wildcard Topic Name {:?} (publish returned {result:?})",
            parse_publish(&packet).topic
        );
    }
}

/// [MQTT-3.3.2-8] The Client does not send a PUBLISH with Topic Alias 0.
#[client_conformance_test(ids = ["MQTT-3.3.2-8"])]
async fn client_must_not_send_topic_alias_zero(sut: ClientSutHandle) {
    let spec = PublishSpec {
        topic_alias: Some(0),
        ..PublishSpec::default()
    };
    let (result, sent) = published(&sut, "pub-alias0", &[], "t/alias", spec).await;
    if let Some(packet) = sent {
        assert_ne!(
            parse_publish(&packet).properties.topic_alias,
            Some(0),
            "[MQTT-3.3.2-8] client sent PUBLISH with Topic Alias 0 (publish returned {result:?})"
        );
    }
}

/// [MQTT-3.3.2-9] The Client does not send a Topic Alias above the Topic
/// Alias Maximum of the CONNACK.
#[client_conformance_test(ids = ["MQTT-3.3.2-9"])]
async fn client_must_not_exceed_server_topic_alias_maximum(sut: ClientSutHandle) {
    let spec = PublishSpec {
        topic_alias: Some(3),
        ..PublishSpec::default()
    };
    let (result, sent) = published(
        &sut,
        "pub-alias3",
        &property_u16(PROP_TOPIC_ALIAS_MAXIMUM, 2),
        "t/alias",
        spec,
    )
    .await;
    if let Some(packet) = sent {
        let alias = parse_publish(&packet).properties.topic_alias;
        assert!(
            alias.is_none_or(|a| a <= 2),
            "[MQTT-3.3.2-9] client sent Topic Alias {alias:?} above the server Topic Alias Maximum 2 (publish returned {result:?})"
        );
    }
}

/// [MQTT-3.3.4-6] A PUBLISH the Client sends carries no Subscription
/// Identifier.
#[client_conformance_test(ids = ["MQTT-3.3.4-6"])]
async fn client_publish_must_not_contain_subscription_identifier(sut: ClientSutHandle) {
    let spec = PublishSpec {
        subscription_identifiers: vec![7],
        ..PublishSpec::default()
    };
    let (result, sent) = published(&sut, "pub-subid", &[], "t/subid", spec).await;
    if let Some(packet) = sent {
        assert!(
            parse_publish(&packet)
                .properties
                .subscription_identifiers
                .is_empty(),
            "[MQTT-3.3.4-6] client sent PUBLISH containing a Subscription Identifier (publish returned {result:?})"
        );
    }
}

/// [MQTT-3.3.2-14] The Response Topic of a PUBLISH the Client sends contains
/// no wildcard characters.
#[client_conformance_test(ids = ["MQTT-3.3.2-14"])]
async fn response_topic_must_not_contain_wildcards(sut: ClientSutHandle) {
    let spec = PublishSpec {
        response_topic: Some("reply/#".to_owned()),
        ..PublishSpec::default()
    };
    let (result, sent) = published(&sut, "pub-resp", &[], "t/req", spec).await;
    if let Some(packet) = sent {
        panic!(
            "[MQTT-3.3.2-14] client sent PUBLISH with wildcard Response Topic {:?} (publish returned {result:?})",
            parse_publish(&packet).properties.response_topic
        );
    }
}

/// [MQTT-3.3.2-1] [MQTT-3.3.2-13] [MQTT-3.3.2-19] The Topic Name, Response
/// Topic and Content Type are UTF-8 Encoded Strings, which cannot contain
/// U+0000, so the Client does not send them with one.
#[client_conformance_test(ids = ["MQTT-3.3.2-1", "MQTT-3.3.2-13", "MQTT-3.3.2-19"])]
async fn utf8_string_fields_reject_nul(sut: ClientSutHandle) {
    let cases = [
        (
            "t/\0nul",
            PublishSpec::default(),
            "MQTT-3.3.2-1",
            "Topic Name",
        ),
        (
            "t/ok",
            PublishSpec {
                response_topic: Some("r/\0".to_owned()),
                ..PublishSpec::default()
            },
            "MQTT-3.3.2-13",
            "Response Topic",
        ),
        (
            "t/ok",
            PublishSpec {
                content_type: Some("text/\0".to_owned()),
                ..PublishSpec::default()
            },
            "MQTT-3.3.2-19",
            "Content Type",
        ),
    ];
    for (topic, spec, statement, field) in cases {
        let (result, sent) = published(&sut, "pub-nul", &[], topic, spec).await;
        assert!(
            sent.is_none(),
            "[{statement}] client sent a PUBLISH whose {field} contains U+0000 (publish returned {result:?})"
        );
    }
}

/// [MQTT-3.3.1-4] A PUBLISH with both `QoS` bits set is malformed: the Client
/// does not deliver it and closes the Network Connection.
#[client_conformance_test(ids = ["MQTT-3.3.1-4"])]
async fn inbound_qos3_publish_is_malformed(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("pub-qos3"), false, &[]).await;
    s.result.as_ref().unwrap();
    subscribe_and_ack(&mut s, "t/q3", 2).await;
    s.conn
        .send(&publish(0x36, b"t/q3", Some(3), &[], b"bad"))
        .await
        .unwrap();
    let delivered = received_after(&s.client, SHORT).await.len();
    let close = s.conn.observe_close(Duration::from_secs(3)).await;
    assert_eq!(
        delivered, 0,
        "[MQTT-3.3.1-4] QoS 3 PUBLISH was delivered to the application"
    );
    assert!(
        close.disconnect_reason.map_or(close.closed, |rc| rc >= 0x80),
        "[MQTT-3.3.1-4] client did not end the connection as malformed after a QoS 3 PUBLISH; closed={} DISCONNECT reason {:?}",
        close.closed,
        close.disconnect_reason
    );
}

/// [MQTT-3.3.2-10] The Client accepts a Topic Alias from 1 up to the Topic
/// Alias Maximum it sent in CONNECT.
#[client_conformance_test(ids = ["MQTT-3.3.2-10"])]
async fn client_accepts_topic_alias_within_its_maximum(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("pub-alias-ok").with_topic_alias_maximum(2);
    let mut s = start(&sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    let connect = ParsedConnect::parse(s.connect.bytes()).expect("client sent a malformed CONNECT");
    assert_eq!(
        connect.properties.topic_alias_maximum,
        Some(2),
        "CONNECT carries Topic Alias Maximum 2"
    );
    subscribe_and_ack(&mut s, "t/a", 0).await;
    s.conn
        .send(&publish(
            0x30,
            b"t/a",
            None,
            &property_u16(PROP_TOPIC_ALIAS, 1),
            b"one",
        ))
        .await
        .unwrap();
    s.conn
        .send(&publish(
            0x30,
            b"",
            None,
            &property_u16(PROP_TOPIC_ALIAS, 1),
            b"two",
        ))
        .await
        .unwrap();
    let topics: Vec<String> = received_after(&s.client, SHORT)
        .await
        .into_iter()
        .map(|message| message.topic)
        .collect();
    assert_eq!(
        topics,
        ["t/a", "t/a"],
        "[MQTT-3.3.2-10] client did not resolve an inbound Topic Alias (1 <= its maximum 2)"
    );
}

async fn alias_violation(
    sut: &ClientSutHandle,
    client_id: &str,
    topic: &[u8],
    alias: u16,
) -> (usize, Option<u8>) {
    let spec = ConnectSpec::new(client_id).with_topic_alias_maximum(2);
    let mut s = start(sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    subscribe_and_ack(&mut s, "t/a", 0).await;
    s.conn
        .send(&publish(
            publish_first_byte(0, false),
            topic,
            None,
            &property_u16(PROP_TOPIC_ALIAS, alias),
            b"x",
        ))
        .await
        .unwrap();
    let delivered = received_after(&s.client, SHORT).await.len();
    let close = s.conn.observe_close(Duration::from_secs(3)).await;
    (delivered, close.disconnect_reason)
}

/// [MQTT-3.3.2-8] An inbound PUBLISH with Topic Alias 0 is a Protocol Error
/// the Client answers with DISCONNECT 0x94 (Topic Alias invalid).
#[client_conformance_test(ids = ["MQTT-3.3.2-8"])]
async fn inbound_topic_alias_zero_is_protocol_error(sut: ClientSutHandle) {
    let (delivered, reason) = alias_violation(&sut, "pub-in-alias0", b"t/a", 0).await;
    assert!(
        delivered == 0 && reason == Some(0x94),
        "[MQTT-3.3.2-8] inbound Topic Alias 0 must draw DISCONNECT 0x94; delivered={delivered} DISCONNECT reason {reason:?}"
    );
}

/// [MQTT-3.3.2-8] An inbound PUBLISH with Topic Alias 0 and a zero-length
/// Topic Name draws DISCONNECT 0x94 (Topic Alias invalid), not 0x82.
#[client_conformance_test(ids = ["MQTT-3.3.2-8"])]
async fn inbound_topic_alias_zero_with_empty_topic_is_topic_alias_invalid(sut: ClientSutHandle) {
    let (delivered, reason) = alias_violation(&sut, "pub-in-alias0-empty", b"", 0).await;
    assert!(
        delivered == 0 && reason == Some(0x94),
        "[MQTT-3.3.2-8] inbound Topic Alias 0 with a zero-length Topic Name must draw DISCONNECT 0x94; delivered={delivered} DISCONNECT reason {reason:?}"
    );
}

/// [MQTT-3.3.2-11] An inbound Topic Alias above the Client's Topic Alias
/// Maximum is a Protocol Error the Client answers with DISCONNECT 0x94.
#[client_conformance_test(ids = ["MQTT-3.3.2-11"])]
async fn inbound_topic_alias_above_maximum_is_protocol_error(sut: ClientSutHandle) {
    let (delivered, reason) = alias_violation(&sut, "pub-in-alias3", b"t/a", 3).await;
    assert!(
        delivered == 0 && reason == Some(0x94),
        "[MQTT-3.3.2-11] inbound Topic Alias 3 above the client maximum 2 must draw DISCONNECT 0x94; delivered={delivered} DISCONNECT reason {reason:?}"
    );
}

/// [MQTT-3.3.2-11] An inbound Topic Alias above the Client's maximum with a
/// zero-length Topic Name draws DISCONNECT 0x94, not 0x82.
#[client_conformance_test(ids = ["MQTT-3.3.2-11"])]
async fn inbound_topic_alias_above_maximum_with_empty_topic_is_topic_alias_invalid(
    sut: ClientSutHandle,
) {
    let (delivered, reason) = alias_violation(&sut, "pub-in-alias3-empty", b"", 3).await;
    assert!(
        delivered == 0 && reason == Some(0x94),
        "[MQTT-3.3.2-11] inbound Topic Alias 3 above the client maximum 2 with a zero-length Topic Name must draw DISCONNECT 0x94; delivered={delivered} DISCONNECT reason {reason:?}"
    );
}
