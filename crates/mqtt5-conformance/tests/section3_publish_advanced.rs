use mqtt5::{PublishOptions, PublishProperties, QoS, SubscribeOptions};
use mqtt5_conformance::harness::{
    connected_client, unique_client_id, ConformanceBroker, MessageCollector,
};
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

#[tokio::test]
async fn overlapping_subs_max_qos_delivered() {
    let broker = ConformanceBroker::start().await;
    let tag = unique_client_id("overlap");
    let topic = format!("overlap/{tag}/data");
    let filter_plus = format!("overlap/{tag}/+");
    let filter_hash = format!("overlap/{tag}/#");

    let sub_id = unique_client_id("sub-oq");
    let mut sub = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        &filter_plus,
        0,
        1,
    ))
    .await
    .unwrap();
    sub.expect_suback(TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        &filter_hash,
        1,
        2,
    ))
    .await
    .unwrap();
    sub.expect_suback(TIMEOUT).await;

    let publisher = connected_client("pub-overlap-qos", &broker).await;
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"hello", pub_opts)
        .await
        .expect("publish failed");

    let mut received = Vec::new();
    for _ in 0..3 {
        if let Some(data) = sub.read_packet_bytes(TIMEOUT).await {
            for pkt in split_mqtt_packets(&data) {
                if !pkt.is_empty() && (pkt[0] & 0xF0) == 0x30 {
                    received.push(pkt);
                }
            }
        }
        if received.len() >= 2 {
            break;
        }
    }

    assert_eq!(
        received.len(),
        2,
        "[MQTT-3.3.4-2] overlapping subscriptions must deliver 2 copies"
    );

    let qos1 = (received[0][0] >> 1) & 0x03;
    let qos2 = (received[1][0] >> 1) & 0x03;
    let max_qos = qos1.max(qos2);
    assert_eq!(
        max_qos, 1,
        "[MQTT-3.3.4-2] at least one copy must be delivered at QoS 1 (max of overlapping)"
    );

    publisher.disconnect().await.expect("disconnect failed");
}

#[tokio::test]
async fn overlapping_subs_subscription_ids_per_copy() {
    let broker = ConformanceBroker::start().await;
    let tag = unique_client_id("subid");
    let topic = format!("subid/{tag}/data");
    let filter_a = format!("subid/{tag}/+");
    let filter_b = format!("subid/{tag}/#");

    let sub_id = unique_client_id("sub-oi");
    let mut sub = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    sub.connect_and_establish(&sub_id, TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe_with_sub_id(
        &filter_a, 1, 1, 10,
    ))
    .await
    .unwrap();
    sub.expect_suback(TIMEOUT).await;
    sub.send_raw(&RawPacketBuilder::subscribe_with_sub_id(
        &filter_b, 1, 2, 20,
    ))
    .await
    .unwrap();
    sub.expect_suback(TIMEOUT).await;

    let publisher = connected_client("pub-overlap-id", &broker).await;
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"identify", pub_opts)
        .await
        .expect("publish failed");

    let mut received = Vec::new();
    for _ in 0..3 {
        if let Some(data) = sub.read_packet_bytes(TIMEOUT).await {
            for pkt in split_mqtt_packets(&data) {
                if !pkt.is_empty() && (pkt[0] & 0xF0) == 0x30 {
                    received.push(pkt);
                }
            }
        }
        if received.len() >= 2 {
            break;
        }
    }

    assert_eq!(
        received.len(),
        2,
        "[MQTT-3.3.4-3] overlapping subscriptions must deliver 2 PUBLISH copies"
    );

    let id1 = extract_subscription_identifier(&received[0]);
    let id2 = extract_subscription_identifier(&received[1]);

    assert!(
        id1.is_some() && id2.is_some(),
        "[MQTT-3.3.4-3] each PUBLISH copy must contain a Subscription Identifier"
    );

    let mut seen_ids = vec![id1.unwrap(), id2.unwrap()];
    seen_ids.sort_unstable();
    assert_eq!(
        seen_ids,
        vec![10, 20],
        "[MQTT-3.3.4-5] each copy must carry its matching subscription identifier"
    );

    publisher.disconnect().await.expect("disconnect failed");
}

#[tokio::test]
async fn overlapping_subs_no_local_prevents_echo() {
    let broker = ConformanceBroker::start().await;
    let tag = unique_client_id("nolocal");
    let topic = format!("nolocal/{tag}/data");
    let filter = format!("nolocal/{tag}/+");

    let self_collector = MessageCollector::new();
    let client = connected_client("client-nolocal", &broker).await;

    let sub_opts = SubscribeOptions {
        qos: QoS::AtLeastOnce,
        no_local: true,
        ..Default::default()
    };
    client
        .subscribe_with_options(&filter, sub_opts, self_collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let other_collector = MessageCollector::new();
    let other = connected_client("other-nolocal", &broker).await;
    let other_sub_opts = SubscribeOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    other
        .subscribe_with_options(&filter, other_sub_opts, other_collector.callback())
        .await
        .expect("other subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    client
        .publish(&topic, b"echo-test")
        .await
        .expect("publish failed");

    assert!(
        other_collector.wait_for_messages(1, TIMEOUT).await,
        "other subscriber should receive the message"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        self_collector.count(),
        0,
        "no_local subscriber must not receive its own publish on wildcard subscription"
    );

    client.disconnect().await.expect("disconnect failed");
    other.disconnect().await.expect("disconnect failed");
}

#[tokio::test]
async fn message_expiry_drops_expired_retained() {
    let broker = ConformanceBroker::start().await;
    let tag = unique_client_id("expiry");
    let topic = format!("expiry/{tag}");

    let publisher = connected_client("pub-expiry", &broker).await;
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        retain: true,
        properties: PublishProperties {
            message_expiry_interval: Some(1),
            ..Default::default()
        },
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"ephemeral", pub_opts)
        .await
        .expect("publish retained failed");
    publisher.disconnect().await.expect("disconnect failed");

    tokio::time::sleep(Duration::from_secs(2)).await;

    let collector = MessageCollector::new();
    let subscriber = connected_client("sub-expiry", &broker).await;
    subscriber
        .subscribe(&topic, collector.callback())
        .await
        .expect("subscribe failed");

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        collector.count(),
        0,
        "[MQTT-3.3.2-5] expired retained message must not be delivered to new subscriber"
    );

    subscriber.disconnect().await.expect("disconnect failed");
}

#[tokio::test]
async fn message_expiry_interval_decremented() {
    let broker = ConformanceBroker::start().await;
    let tag = unique_client_id("decrement");
    let topic = format!("decrement/{tag}");

    let publisher = connected_client("pub-decrement", &broker).await;
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        retain: true,
        properties: PublishProperties {
            message_expiry_interval: Some(30),
            ..Default::default()
        },
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"aging", pub_opts)
        .await
        .expect("publish retained failed");
    publisher.disconnect().await.expect("disconnect failed");

    tokio::time::sleep(Duration::from_secs(2)).await;

    let collector = MessageCollector::new();
    let subscriber = connected_client("sub-decrement", &broker).await;
    subscriber
        .subscribe(&topic, collector.callback())
        .await
        .expect("subscribe failed");

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "retained message should be delivered"
    );

    let msgs = collector.get_messages();
    let expiry = msgs[0]
        .properties
        .message_expiry_interval
        .expect("[MQTT-3.3.2-6] delivered retained message must include message_expiry_interval");
    assert!(
        expiry < 30,
        "[MQTT-3.3.2-6] message_expiry_interval must be decremented (got {expiry}, expected < 30)"
    );

    subscriber.disconnect().await.expect("disconnect failed");
}

#[tokio::test]
async fn response_topic_wildcard_rejected() {
    let broker = ConformanceBroker::start().await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("rt-wc");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_qos0_with_response_topic(
        "test/topic",
        b"payload",
        "reply/#",
    ))
    .await
    .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.3.2-14] broker must disconnect client that sends PUBLISH with wildcard in Response Topic"
    );
}

#[tokio::test]
async fn response_topic_valid_utf8_forwarded() {
    let broker = ConformanceBroker::start().await;
    let tag = unique_client_id("rt-fwd");
    let topic = format!("rtfwd/{tag}");

    let collector = MessageCollector::new();
    let subscriber = connected_client("sub-rt-fwd", &broker).await;
    subscriber
        .subscribe(&topic, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("pub-rt-fwd", &broker).await;
    let pub_opts = PublishOptions {
        qos: QoS::AtMostOnce,
        properties: PublishProperties {
            response_topic: Some("reply/topic".to_owned()),
            ..Default::default()
        },
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"with-rt", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "subscriber should receive message with response topic"
    );

    let msgs = collector.get_messages();
    assert_eq!(
        msgs[0].properties.response_topic.as_deref(),
        Some("reply/topic"),
        "[MQTT-3.3.2-13] valid UTF-8 Response Topic must be forwarded to subscriber"
    );

    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

fn split_mqtt_packets(data: &[u8]) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        if offset + 1 >= data.len() {
            break;
        }
        let start = offset;
        offset += 1;
        let mut remaining_len: u32 = 0;
        let mut shift = 0;
        loop {
            if offset >= data.len() {
                return packets;
            }
            let byte = data[offset];
            offset += 1;
            remaining_len |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 21 {
                return packets;
            }
        }
        let end = offset + remaining_len as usize;
        if end > data.len() {
            break;
        }
        packets.push(data[start..end].to_vec());
        offset = end;
    }
    packets
}

fn decode_variable_int(data: &[u8], start: usize) -> Option<(u32, usize)> {
    let mut value: u32 = 0;
    let mut shift = 0;
    let mut idx = start;
    loop {
        if idx >= data.len() {
            return None;
        }
        let byte = data[idx];
        idx += 1;
        value |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Some((value, idx));
        }
        shift += 7;
        if shift > 21 {
            return None;
        }
    }
}

fn extract_subscription_identifier(data: &[u8]) -> Option<u32> {
    if data.is_empty() || (data[0] & 0xF0) != 0x30 {
        return None;
    }
    let qos = (data[0] >> 1) & 0x03;
    let (remaining_len, mut idx) = decode_variable_int(data, 1)?;
    let payload_end = idx + remaining_len as usize;
    if data.len() < payload_end {
        return None;
    }
    if idx + 2 > data.len() {
        return None;
    }
    let topic_len = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
    idx += 2 + topic_len;
    if qos > 0 {
        idx += 2;
    }
    let (props_len, props_start) = decode_variable_int(data, idx)?;
    let props_end = props_start + props_len as usize;
    idx = props_start;
    while idx < props_end {
        let prop_id = data[idx];
        idx += 1;
        if prop_id == 0x0B {
            let (val, _) = decode_variable_int(data, idx)?;
            return Some(val);
        }
        match prop_id {
            0x01 => idx += 1,
            0x02 => idx += 4,
            0x23 => idx += 2,
            0x08 | 0x03 | 0x09 | 0x1A | 0x26 => {
                if idx + 2 > data.len() {
                    return None;
                }
                let len = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
                idx += 2 + len;
                if prop_id == 0x26 {
                    if idx + 2 > data.len() {
                        return None;
                    }
                    let vlen = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
                    idx += 2 + vlen;
                }
            }
            _ => return None,
        }
    }
    None
}
