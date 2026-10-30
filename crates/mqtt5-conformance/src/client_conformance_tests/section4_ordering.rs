use super::common::{
    accept_next, ack_ids, drop_connection, packets_until_quiet, parse_publish, start,
    subscribe_and_ack, subscribe_deferred_and_ack, wait_for_received, TIMEOUT,
};
use crate::client_conformance_test;
use crate::client_sut::{ClientSutHandle, ConnectSpec, PublishSpec, Settle};
use crate::packet_parser::ParsedConnect;
use crate::scripted_broker::{
    ack, publish, publish_first_byte, BrokerConnection, Packet, PUBACK, PUBLISH, PUBREC, PUBREL,
};
use std::time::Duration;

async fn send_burst(conn: &mut BrokerConnection, qos: u8, topic: &[u8], ids: &[u16]) {
    let burst: Vec<u8> = ids
        .iter()
        .flat_map(|id| publish(publish_first_byte(qos, false), topic, Some(*id), &[], b"x"))
        .collect();
    conn.send(&burst).await.unwrap();
}

/// [MQTT-4.6.0-2] [MQTT-4.6.0-3] The Client sends PUBACK and PUBREC in the
/// order the PUBLISH packets arrived.
#[client_conformance_test(ids = ["MQTT-4.6.0-2", "MQTT-4.6.0-3"])]
async fn automatic_acks_follow_publish_order(sut: ClientSutHandle) {
    let mut s = start(&sut, ConnectSpec::new("order-auto"), false, &[]).await;
    s.result.as_ref().unwrap();
    subscribe_and_ack(&mut s, "t/#", 2).await;
    let qos1: Vec<u16> = (1..=6).collect();
    send_burst(&mut s.conn, 1, b"t/a", &qos1).await;
    assert_eq!(
        ack_ids(&mut s.conn, PUBACK, qos1.len()).await,
        qos1,
        "[MQTT-4.6.0-2] PUBACKs not in PUBLISH order"
    );
    let qos2: Vec<u16> = (20..=25).collect();
    send_burst(&mut s.conn, 2, b"t/b", &qos2).await;
    assert_eq!(
        ack_ids(&mut s.conn, PUBREC, qos2.len()).await,
        qos2,
        "[MQTT-4.6.0-3] PUBRECs not in PUBLISH order"
    );
}

async fn deferred_settled_newest_first(sut: &ClientSutHandle, qos: u8, ack_type: u8) -> Vec<u16> {
    let spec = ConnectSpec::new(&format!("order-deferred-{qos}"))
        .resuming(300)
        .with_receive_maximum(16)
        .with_deferred_ack();
    let mut s = start(sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    subscribe_deferred_and_ack(&mut s, "t/d", qos).await;
    send_burst(&mut s.conn, qos, b"t/d", &[1, 2]).await;
    assert_eq!(
        wait_for_received(&s.client, 2).await.len(),
        2,
        "both messages were delivered"
    );
    assert!(
        s.client.settle_newest_deferred(Settle::Ack),
        "the second message holds a token"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        s.client.settle_deferred(Settle::Ack),
        1,
        "the first message holds a token"
    );
    ack_ids(&mut s.conn, ack_type, 2).await
}

/// [MQTT-4.6.0-2] Deferred acknowledgements settled newest first still reach
/// the wire as PUBACKs in PUBLISH order.
#[client_conformance_test(ids = ["MQTT-4.6.0-2"], requires = ["client.deferred_ack"])]
async fn deferred_pubacks_follow_publish_order(sut: ClientSutHandle) {
    assert_eq!(
        deferred_settled_newest_first(&sut, 1, PUBACK).await,
        [1, 2],
        "[MQTT-4.6.0-2] PUBACKs sent in settle order, not PUBLISH order"
    );
}

/// [MQTT-4.6.0-3] Deferred acknowledgements settled newest first still reach
/// the wire as PUBRECs in PUBLISH order.
#[client_conformance_test(ids = ["MQTT-4.6.0-3"], requires = ["client.deferred_ack"])]
async fn deferred_pubrecs_follow_publish_order(sut: ClientSutHandle) {
    assert_eq!(
        deferred_settled_newest_first(&sut, 2, PUBREC).await,
        [1, 2],
        "[MQTT-4.6.0-3] PUBRECs sent in settle order, not PUBLISH order"
    );
}

/// [MQTT-4.6.0-2] An automatic PUBACK waits behind an earlier message whose
/// acknowledgement is still deferred.
#[client_conformance_test(ids = ["MQTT-4.6.0-2"], requires = ["client.deferred_ack"])]
async fn automatic_puback_waits_for_earlier_deferred_one(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("order-mixed")
        .resuming(300)
        .with_receive_maximum(16)
        .with_deferred_ack();
    let mut s = start(&sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    subscribe_deferred_and_ack(&mut s, "t/deferred", 1).await;
    subscribe_and_ack(&mut s, "t/auto", 1).await;
    send_burst(&mut s.conn, 1, b"t/deferred", &[1]).await;
    send_burst(&mut s.conn, 1, b"t/auto", &[2]).await;
    assert_eq!(
        wait_for_received(&s.client, 2).await.len(),
        2,
        "both messages were delivered"
    );
    assert_eq!(
        s.client.settle_deferred(Settle::Ack),
        1,
        "the deferred message holds a token"
    );
    assert_eq!(
        ack_ids(&mut s.conn, PUBACK, 2).await,
        [1, 2],
        "[MQTT-4.6.0-2] the automatic PUBACK overtook the deferred one"
    );
}

/// [MQTT-4.4.0-1] [MQTT-4.6.0-1] After a session resumes, the Client resends
/// its unacknowledged `QoS` 1 and `QoS` 2 PUBLISH packets in the order it
/// first sent them.
#[client_conformance_test(
    ids = ["MQTT-4.4.0-1", "MQTT-4.6.0-1"],
    requires = ["client.automatic_reconnect"],
)]
async fn resent_publishes_keep_original_order(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("order-resend")
        .reconnecting()
        .resuming(300);
    let mut s = start(&sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    let mut sent = Vec::new();
    let mut publishing = Vec::new();
    for (i, qos) in [1u8, 2, 1].into_iter().enumerate() {
        publishing.push(tokio::spawn(s.client.publish(
            "t/resend",
            &[u8::try_from(i).unwrap()],
            PublishSpec::qos(qos),
        )));
        let packet = s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap();
        sent.push(parse_publish(&packet).packet_id.unwrap());
    }
    let (client, broker) = drop_connection(s).await;
    let (mut conn, connect) = accept_next(&broker, true, &[]).await;
    let resumed = ParsedConnect::parse(connect.bytes()).expect("client sent a malformed CONNECT");
    assert_eq!(resumed.flags & 0x02, 0, "the reconnect keeps Clean Start 0");
    let resent: Vec<u16> = packets_until_quiet(&mut conn, Duration::from_secs(3))
        .await
        .iter()
        .filter(|p| p.packet_type() == PUBLISH)
        .filter_map(|p| parse_publish(p).packet_id)
        .collect();
    drop(client);
    publishing.iter().for_each(tokio::task::JoinHandle::abort);
    let mut resent_set = resent.clone();
    resent_set.sort_unstable();
    let mut sent_set = sent.clone();
    sent_set.sort_unstable();
    assert_eq!(
        resent_set, sent_set,
        "[MQTT-4.4.0-1] unacknowledged PUBLISH ids {sent:?} not all resent after Session Present 1: {resent:?}"
    );
    assert_eq!(
        resent, sent,
        "[MQTT-4.6.0-1] PUBLISH packets resent out of their original order"
    );
}

/// [MQTT-4.6.0-4] [MQTT-4.4.0-1] [MQTT-4.3.3-6] PUBREL packets follow the
/// order of the PUBRECs, also when the outstanding ones are resent after the
/// session resumes, and no PUBLISH is resent once its PUBREL was sent.
#[client_conformance_test(
    ids = ["MQTT-4.6.0-4", "MQTT-4.4.0-1", "MQTT-4.3.3-6"],
    requires = ["client.automatic_reconnect"],
)]
async fn pubrel_order_follows_pubrec_order(sut: ClientSutHandle) {
    let spec = ConnectSpec::new("order-pubrel")
        .reconnecting()
        .resuming(300);
    let mut s = start(&sut, spec, false, &[]).await;
    s.result.as_ref().unwrap();
    let mut ids = Vec::new();
    let mut publishing = Vec::new();
    for i in 0..3u8 {
        publishing.push(tokio::spawn(s.client.publish(
            "t/rel",
            &[i],
            PublishSpec::qos(2),
        )));
        let packet = s.conn.next_of_type(PUBLISH, TIMEOUT).await.unwrap();
        ids.push(parse_publish(&packet).packet_id.unwrap());
    }
    let pubrec_order = [ids[2], ids[0], ids[1]];
    let burst: Vec<u8> = pubrec_order.iter().flat_map(|id| ack(0x50, *id)).collect();
    s.conn.send(&burst).await.unwrap();
    assert_eq!(
        ack_ids(&mut s.conn, PUBREL, 3).await,
        pubrec_order,
        "[MQTT-4.6.0-4] PUBRELs not in PUBREC order"
    );
    s.conn.send(&ack(0x70, ids[0])).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (client, broker) = drop_connection(s).await;
    let (mut conn, _) = accept_next(&broker, true, &[]).await;
    let packets = packets_until_quiet(&mut conn, Duration::from_secs(3)).await;
    drop(client);
    publishing.iter().for_each(tokio::task::JoinHandle::abort);
    let types: Vec<u8> = packets.iter().map(Packet::packet_type).collect();
    assert!(
        !types.contains(&PUBLISH),
        "[MQTT-4.3.3-6] PUBLISH resent after its PUBREL was sent (packet types {types:?})"
    );
    let rels: Vec<u16> = packets
        .iter()
        .filter(|p| p.packet_type() == PUBREL)
        .filter_map(Packet::leading_packet_id)
        .collect();
    let mut rel_set = rels.clone();
    rel_set.sort_unstable();
    let mut outstanding = vec![ids[2], ids[1]];
    outstanding.sort_unstable();
    assert_eq!(
        rel_set, outstanding,
        "[MQTT-4.4.0-1] outstanding PUBRELs not resent after Session Present 1: {rels:?}"
    );
    assert_eq!(
        rels,
        [ids[2], ids[1]],
        "[MQTT-4.6.0-4] resent PUBRELs not in PUBREC order"
    );
}
