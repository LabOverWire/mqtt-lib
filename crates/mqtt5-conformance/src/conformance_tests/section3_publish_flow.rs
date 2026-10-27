//! Section 3.3 — PUBLISH flow control (receive maximum).

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use std::time::Duration;

/// `[MQTT-3.3.4-9]` `[MQTT-4.9.0-1]` `[MQTT-4.9.0-2]` The Server MUST NOT send
/// more than Receive Maximum `QoS` 1 and `QoS` 2 PUBLISH packets for which it
/// has not received PUBACK, PUBCOMP, or PUBREC with a Reason Code of 128 or
/// greater from the Client.
#[conformance_test(
    ids = ["MQTT-3.3.4-9", "MQTT-4.9.0-1", "MQTT-4.9.0-2"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn receive_maximum_limits_outbound_publishes(sut: SutHandle) {
    let topic = format!("recv-max/{}", unique_client_id("flow"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub-rm");
    sub.send_raw(&RawPacketBuilder::connect_with_receive_maximum(&sub_id, 2))
        .await
        .unwrap();
    let connack = sub.expect_connack(Duration::from_secs(2)).await;
    assert!(connack.is_some(), "Subscriber must receive CONNACK");
    let (_, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "Subscriber CONNACK must be Success");

    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 1))
        .await
        .unwrap();
    let suback = sub.expect_suback(Duration::from_secs(2)).await;
    assert!(suback.is_some(), "Must receive SUBACK");

    let mut pub_raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("pub-rm");
    pub_raw
        .send_raw(&RawPacketBuilder::valid_connect(&pub_id))
        .await
        .unwrap();
    let pub_connack = pub_raw.expect_connack(Duration::from_secs(2)).await;
    assert!(pub_connack.is_some(), "Publisher must receive CONNACK");

    for i in 1..=4u16 {
        pub_raw
            .send_raw(&RawPacketBuilder::publish_qos1(
                &topic,
                &[u8::try_from(i).unwrap()],
                i,
            ))
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let publish_count = count_publish_packets_from_raw(&mut sub, Duration::from_secs(2)).await;

    assert_eq!(
        publish_count, 2,
        "[MQTT-3.3.4-9] Server must not send more than receive_maximum (2) unACKed QoS 1 publishes"
    );
}

async fn count_publish_packets_from_raw(client: &mut RawMqttClient, timeout: Duration) -> u32 {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut count = 0;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return count;
        }
        match client
            .read_packet_bytes(remaining.min(Duration::from_millis(500)))
            .await
        {
            Some(data) if data[0] >> 4 == 3 => count += 1,
            Some(_) => {}
            None => return count,
        }
    }
}
