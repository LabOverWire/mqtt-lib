use mqtt5::{PublishOptions, QoS};
use mqtt5_conformance::harness::{connected_client, unique_client_id, ConformanceBroker};
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

#[tokio::test]
async fn flow_control_quota_enforced() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("fc-quota/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub-fc");
    sub.send_raw(&RawPacketBuilder::connect_with_receive_maximum(&sub_id, 2))
        .await
        .unwrap();
    let connack = sub.expect_connack(TIMEOUT).await;
    assert!(connack.is_some(), "must receive CONNACK");
    let (_, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "CONNACK must be Success");

    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 1))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await;

    let mut pub_raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("pub-fc");
    pub_raw
        .send_raw(&RawPacketBuilder::valid_connect(&pub_id))
        .await
        .unwrap();
    let _ = pub_raw.expect_connack(TIMEOUT).await;

    for i in 1..=5u16 {
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

    let accumulated = read_all_available(&mut sub, Duration::from_secs(2)).await;
    let (publish_count, packet_ids) = extract_publish_ids(&accumulated);

    assert_eq!(
        publish_count, 2,
        "[MQTT-4.9.0-1] [MQTT-4.9.0-2] broker must send exactly 2 unACKed QoS 1 publishes (receive_maximum=2)"
    );

    for pid in &packet_ids {
        sub.send_raw(&RawPacketBuilder::puback(*pid)).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let after_data = read_all_available(&mut sub, Duration::from_secs(2)).await;
    let (after_count, after_ids) = extract_publish_ids(&after_data);
    assert!(
        after_count > 0,
        "[MQTT-4.9.0-1] after PUBACKing, broker must send queued messages"
    );

    for pid in &after_ids {
        sub.send_raw(&RawPacketBuilder::puback(*pid)).await.unwrap();
    }
}

#[tokio::test]
async fn flow_control_other_packets_at_zero_quota() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("fc-zero/{}", unique_client_id("t"));
    let topic2 = format!("fc-zero2/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub-fc0");
    sub.send_raw(&RawPacketBuilder::connect_with_receive_maximum(&sub_id, 1))
        .await
        .unwrap();
    let connack = sub.expect_connack(TIMEOUT).await;
    assert!(connack.is_some(), "must receive CONNACK");
    let (_, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "CONNACK must be Success");

    sub.send_raw(&RawPacketBuilder::subscribe(&topic, 1))
        .await
        .unwrap();
    let _ = sub.expect_suback(TIMEOUT).await;

    let publisher = connected_client("pub-fc0", &broker).await;
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"fill-quota", pub_opts.clone())
        .await
        .unwrap();
    publisher
        .publish_with_options(&topic, b"queued", pub_opts)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let initial_data = read_all_available(&mut sub, Duration::from_secs(1)).await;
    let (initial_count, initial_ids) = extract_publish_ids(&initial_data);
    assert_eq!(
        initial_count, 1,
        "broker must send only 1 QoS 1 PUBLISH when receive_maximum=1"
    );
    let first_pid = initial_ids[0];

    sub.send_raw(&RawPacketBuilder::pingreq()).await.unwrap();
    assert!(
        sub.expect_pingresp(TIMEOUT).await,
        "[MQTT-4.9.0-3] PINGREQ must be answered even at zero quota"
    );

    sub.send_raw(&RawPacketBuilder::subscribe_with_packet_id(&topic2, 0, 2))
        .await
        .unwrap();
    let suback2 = sub.expect_suback(TIMEOUT).await;
    assert!(
        suback2.is_some(),
        "[MQTT-4.9.0-3] SUBSCRIBE must be processed even at zero quota"
    );

    sub.send_raw(&RawPacketBuilder::puback(first_pid))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let after_data = read_all_available(&mut sub, Duration::from_secs(2)).await;
    let (after_count, _) = extract_publish_ids(&after_data);
    assert!(
        after_count > 0,
        "after PUBACK, broker must deliver the queued message"
    );
}

#[tokio::test]
async fn auth_invalid_flags_malformed() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("auth-flags");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::auth_with_invalid_flags())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.15.1-1] AUTH with non-zero reserved flags must cause disconnect"
    );
}

async fn read_all_available(client: &mut RawMqttClient, timeout: Duration) -> Vec<u8> {
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

    accumulated
}

fn extract_publish_ids(data: &[u8]) -> (u32, Vec<u16>) {
    let mut count = 0;
    let mut ids = Vec::new();
    let mut idx = 0;

    while idx < data.len() {
        let first_byte = data[idx];
        let packet_type = first_byte >> 4;
        let qos = (first_byte >> 1) & 0x03;
        idx += 1;

        let mut remaining_len: u32 = 0;
        let mut shift = 0;
        loop {
            if idx >= data.len() {
                return (count, ids);
            }
            let byte = data[idx];
            idx += 1;
            remaining_len |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 21 {
                return (count, ids);
            }
        }

        let body_start = idx;
        let body_end = idx + remaining_len as usize;
        if body_end > data.len() {
            if packet_type == 3 {
                count += 1;
            }
            return (count, ids);
        }

        if packet_type == 3 {
            count += 1;
            if qos > 0 && body_start + 2 <= body_end {
                let topic_len =
                    u16::from_be_bytes([data[body_start], data[body_start + 1]]) as usize;
                let pid_offset = body_start + 2 + topic_len;
                if pid_offset + 2 <= body_end {
                    let packet_id = u16::from_be_bytes([data[pid_offset], data[pid_offset + 1]]);
                    ids.push(packet_id);
                }
            }
        }
        idx = body_end;
    }

    (count, ids)
}
