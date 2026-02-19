#![allow(clippy::cast_possible_truncation)]

mod common;

use common::{unique_client_id, ConformanceBroker, RawMqttClient, RawPacketBuilder};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use std::net::SocketAddr;
use std::time::Duration;

fn memory_config() -> BrokerConfig {
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config)
}

/// `[MQTT-3.3.4-7]` The Server MUST NOT send more than Receive Maximum
/// `QoS` 1 and `QoS` 2 PUBLISH packets for which it has not received PUBACK,
/// PUBCOMP, or PUBREC with a Reason Code of 128 or greater from the Client.
///
/// A raw subscriber connects with `receive_maximum=2`. A raw publisher sends
/// 4 `QoS` 1 messages in quick succession. The subscriber reads without sending
/// PUBACKs and verifies that only 2 PUBLISH packets arrive within a reasonable
/// window (the broker queues the rest pending acknowledgement).
#[tokio::test]
async fn receive_maximum_limits_outbound_publishes() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("recv-max/{}", unique_client_id("flow"));

    let mut sub = RawMqttClient::connect_tcp(broker.socket_addr())
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

    let mut pub_raw = RawMqttClient::connect_tcp(broker.socket_addr())
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
            .send_raw(&RawPacketBuilder::publish_qos1(&topic, &[i as u8], i))
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let publish_count = count_publish_packets_from_raw(&mut sub, Duration::from_secs(2)).await;

    assert_eq!(
        publish_count, 2,
        "[MQTT-3.3.4-7] Server must not send more than receive_maximum (2) unACKed QoS 1 publishes"
    );
}

async fn count_publish_packets_from_raw(client: &mut RawMqttClient, timeout: Duration) -> u32 {
    let mut accumulated = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match client
            .read_packet_bytes(remaining.min(Duration::from_millis(500)))
            .await
        {
            Some(data) => accumulated.extend_from_slice(&data),
            None => break,
        }
    }

    count_publish_headers(&accumulated)
}

fn count_publish_headers(data: &[u8]) -> u32 {
    let mut count = 0;
    let mut idx = 0;
    while idx < data.len() {
        let first_byte = data[idx];
        let packet_type = first_byte >> 4;
        idx += 1;

        let mut remaining_len: u32 = 0;
        let mut shift = 0;
        loop {
            if idx >= data.len() {
                return count;
            }
            let byte = data[idx];
            idx += 1;
            remaining_len |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 21 {
                return count;
            }
        }

        let body_end = idx + remaining_len as usize;
        if body_end > data.len() {
            if packet_type == 3 {
                count += 1;
            }
            return count;
        }

        if packet_type == 3 {
            count += 1;
        }
        idx = body_end;
    }
    count
}

#[tokio::test]
async fn inbound_receive_maximum_exceeded_disconnects_with_0x93() {
    let config = memory_config().with_server_receive_maximum(2);
    let broker = ConformanceBroker::start_with_config(config).await;

    let mut client = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let cid = unique_client_id("recv-max-in");
    client
        .send_raw(&RawPacketBuilder::valid_connect(&cid))
        .await
        .unwrap();
    let connack = client.expect_connack(Duration::from_secs(2)).await;
    assert!(connack.is_some(), "must receive CONNACK");

    client
        .send_raw(&RawPacketBuilder::publish_qos2("test/rm", &[1], 1))
        .await
        .unwrap();
    let pubrec1 = client.expect_pubrec(Duration::from_secs(2)).await;
    assert!(pubrec1.is_some(), "must receive PUBREC for packet 1");

    client
        .send_raw(&RawPacketBuilder::publish_qos2("test/rm", &[2], 2))
        .await
        .unwrap();
    let pubrec2 = client.expect_pubrec(Duration::from_secs(2)).await;
    assert!(pubrec2.is_some(), "must receive PUBREC for packet 2");

    client
        .send_raw(&RawPacketBuilder::publish_qos2("test/rm", &[3], 3))
        .await
        .unwrap();

    let reason = client
        .expect_disconnect_packet(Duration::from_secs(2))
        .await;
    assert_eq!(
        reason,
        Some(0x93),
        "[MQTT-3.3.4-8] Server must send DISCONNECT 0x93 when inbound receive maximum exceeded"
    );
}
