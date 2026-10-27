//! Section 4.9 — Flow Control (receive maximum quota enforcement).

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::packet_parser::ParsedPublish;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use crate::test_client::TestClient;
use mqtt5_protocol::types::{PublishOptions, QoS};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// `[MQTT-3.3.4-9]` `[MQTT-4.9.0-1]` `[MQTT-4.9.0-2]` The Server must not send
/// more unacknowledged `QoS` 1 and `QoS` 2 PUBLISH packets than the Client's
/// Receive Maximum. Further queued messages are delivered only after PUBACKs
/// free the quota.
#[conformance_test(
    ids = ["MQTT-3.3.4-9", "MQTT-4.9.0-1", "MQTT-4.9.0-2"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn flow_control_quota_enforced(sut: SutHandle) {
    let topic = format!("fc-quota/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

    let mut pub_raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

    let publishes = sub.read_publish_packets(Duration::from_secs(2)).await;

    assert_eq!(
        publishes.len(),
        2,
        "[MQTT-4.9.0-1] [MQTT-4.9.0-2] broker must send exactly 2 unACKed QoS 1 publishes (receive_maximum=2)"
    );

    for pid in publishes
        .iter()
        .filter_map(|p| ParsedPublish::parse(p)?.packet_id)
    {
        sub.send_raw(&RawPacketBuilder::puback(pid)).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let after = sub.read_publish_packets(Duration::from_secs(2)).await;
    assert!(
        !after.is_empty(),
        "after PUBACKing, broker must send queued messages"
    );

    for pid in after
        .iter()
        .filter_map(|p| ParsedPublish::parse(p)?.packet_id)
    {
        sub.send_raw(&RawPacketBuilder::puback(pid)).await.unwrap();
    }
}

/// `[MQTT-4.9.0-3]` `[MQTT-3.3.4-10]` PINGREQ, SUBSCRIBE and other non-PUBLISH
/// packets must still be answered without delay when the PUBLISH quota is used
/// up.
#[conformance_test(
    ids = ["MQTT-4.9.0-3", "MQTT-3.3.4-10"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn flow_control_other_packets_at_zero_quota(sut: SutHandle) {
    let topic = format!("fc-zero/{}", unique_client_id("t"));
    let topic2 = format!("fc-zero2/{}", unique_client_id("t"));

    let mut sub = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

    let publisher = TestClient::connect_with_prefix(&sut, "pub-fc0")
        .await
        .unwrap();
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

    let initial = sub.read_publish_packets(Duration::from_secs(1)).await;
    assert_eq!(
        initial.len(),
        1,
        "broker must send only 1 QoS 1 PUBLISH when receive_maximum=1"
    );
    let first_pid = ParsedPublish::parse(&initial[0])
        .and_then(|p| p.packet_id)
        .expect("QoS 1 PUBLISH must parse and carry a packet identifier");

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

    let after = sub.read_publish_packets(Duration::from_secs(2)).await;
    assert!(
        !after.is_empty(),
        "after PUBACK, broker must deliver the queued message"
    );
}

/// `[MQTT-3.15.1-1]` AUTH packet with non-zero reserved flags is a
/// Malformed Packet and MUST cause disconnection.
#[conformance_test(
    ids = ["MQTT-3.15.1-1"],
    requires = ["transport.tcp"],
)]
async fn auth_invalid_flags_malformed(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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
