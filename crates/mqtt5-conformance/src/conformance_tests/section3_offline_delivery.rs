use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::packet_parser::{ParsedConnAck, ParsedPublish};
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
const SESSION_EXPIRY: u32 = 300;
const PROBE: &[u8] = b"probe";
const MARKER: &[u8] = b"marker";
const MAX_READS: usize = 8;

enum Probe {
    Qos0,
    Qos1Retained,
    Qos1Dup,
    Qos2,
}

async fn publish_probe(sut: &SutHandle, topic: &str, probe: Probe) {
    let mut publisher = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    publisher
        .connect_and_establish(&unique_client_id("offline-pub"), TIMEOUT)
        .await;
    match probe {
        Probe::Qos0 => {
            publisher
                .send_raw(&RawPacketBuilder::publish_qos0(topic, PROBE))
                .await
                .unwrap();
        }
        Probe::Qos1Retained | Probe::Qos1Dup => {
            let packet = match probe {
                Probe::Qos1Retained => RawPacketBuilder::publish_qos1_retained(topic, PROBE, 1),
                _ => RawPacketBuilder::publish_qos1_with_dup(topic, PROBE, 1),
            };
            publisher.send_raw(&packet).await.unwrap();
            publisher
                .expect_puback(TIMEOUT)
                .await
                .expect("probe PUBACK");
        }
        Probe::Qos2 => {
            publisher
                .send_raw(&RawPacketBuilder::publish_qos2(topic, PROBE, 1))
                .await
                .unwrap();
            publisher
                .expect_pubrec(TIMEOUT)
                .await
                .expect("probe PUBREC");
            publisher
                .send_raw(&RawPacketBuilder::pubrel(1))
                .await
                .unwrap();
            publisher
                .expect_pubcomp(TIMEOUT)
                .await
                .expect("probe PUBCOMP");
        }
    }
    publisher
        .send_raw(&RawPacketBuilder::publish_qos1(topic, MARKER, 2))
        .await
        .unwrap();
    publisher
        .expect_puback(TIMEOUT)
        .await
        .expect("marker PUBACK");
    publisher
        .send_raw(&RawPacketBuilder::disconnect_normal())
        .await
        .unwrap();
}

async fn offline_delivery(
    sut: &SutHandle,
    filter: &str,
    topic: &str,
    probe: Probe,
) -> Option<ParsedPublish> {
    let client_id = unique_client_id("offline-sub");
    let mut member = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    member
        .send_raw(&RawPacketBuilder::connect_with_session_expiry(
            &client_id,
            SESSION_EXPIRY,
        ))
        .await
        .unwrap();
    let (_, reason) = member.expect_connack(TIMEOUT).await.expect("CONNACK");
    assert_eq!(reason, 0x00, "subscriber connects");
    member
        .send_raw(&RawPacketBuilder::subscribe_with_packet_id(filter, 1, 1))
        .await
        .unwrap();
    let (_, granted) = member.expect_suback(TIMEOUT).await.expect("SUBACK");
    assert_eq!(granted, [0x01], "subscription granted at QoS 1");
    member
        .send_raw(&RawPacketBuilder::disconnect_normal())
        .await
        .unwrap();
    drop(member);
    tokio::time::sleep(Duration::from_millis(200)).await;

    publish_probe(sut, topic, probe).await;

    let mut member = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    member
        .send_raw(&RawPacketBuilder::connect_resume(&client_id, 0))
        .await
        .unwrap();
    let connack = member
        .read_mqtt_packet(TIMEOUT)
        .await
        .as_deref()
        .and_then(ParsedConnAck::parse)
        .expect("CONNACK");
    assert_eq!(connack.reason_code, 0x00, "subscriber reconnects");
    assert!(connack.session_present, "session is present");

    let mut delivered = None;
    for _ in 0..MAX_READS {
        let publish = member
            .read_mqtt_packet(TIMEOUT)
            .await
            .as_deref()
            .and_then(ParsedPublish::parse)
            .expect("queued marker is delivered");
        if let Some(packet_id) = publish.packet_id {
            member
                .send_raw(&RawPacketBuilder::puback(packet_id))
                .await
                .unwrap();
        }
        if publish.payload == MARKER {
            return delivered;
        }
        if publish.payload == PROBE {
            delivered = Some(publish);
        }
    }
    panic!("marker not delivered within {MAX_READS} PUBLISH packets");
}

fn shared(topic: &str) -> String {
    format!("$share/{}/{topic}", unique_client_id("grp"))
}

#[conformance_test(
    ids = ["MQTT-3.8.4-8"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn offline_queued_qos0_is_not_upgraded(sut: SutHandle) {
    let topic = format!("offline/qos0/{}", unique_client_id("t"));
    if let Some(message) = offline_delivery(&sut, &topic, &topic, Probe::Qos0).await {
        assert_eq!(
            message.qos, 0,
            "[MQTT-3.8.4-8] QoS 0 message queued for a QoS 1 subscription must stay QoS 0"
        );
    }
}

#[conformance_test(
    ids = ["MQTT-3.8.4-8"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn offline_queued_qos2_is_downgraded(sut: SutHandle) {
    let topic = format!("offline/qos2/{}", unique_client_id("t"));
    let message = offline_delivery(&sut, &topic, &topic, Probe::Qos2)
        .await
        .expect("QoS 2 message is queued for the offline subscriber");
    assert_eq!(
        message.qos, 1,
        "[MQTT-3.8.4-8] QoS 2 message queued for a QoS 1 subscription must be delivered at QoS 1"
    );
}

#[conformance_test(
    ids = ["MQTT-3.3.1-12"],
    requires = ["transport.tcp", "retain_available", "max_qos>=1"],
)]
async fn offline_queued_retain_is_cleared(sut: SutHandle) {
    let topic = format!("offline/retain/{}", unique_client_id("t"));
    let message = offline_delivery(&sut, &topic, &topic, Probe::Qos1Retained)
        .await
        .expect("retained message is queued for the offline subscriber");
    assert!(
        !message.retain,
        "[MQTT-3.3.1-12] Retain As Published 0 must clear RETAIN on queued delivery"
    );
}

#[conformance_test(
    ids = ["MQTT-3.3.1-3"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn offline_queued_dup_is_cleared(sut: SutHandle) {
    let topic = format!("offline/dup/{}", unique_client_id("t"));
    let message = offline_delivery(&sut, &topic, &topic, Probe::Qos1Dup)
        .await
        .expect("DUP message is queued for the offline subscriber");
    assert!(
        !message.dup,
        "[MQTT-3.3.1-3] first delivery of a queued message must have DUP 0"
    );
}

#[conformance_test(
    ids = ["MQTT-4.8.2-3"],
    requires = ["transport.tcp", "shared_subscription_available", "max_qos>=1"],
)]
async fn offline_shared_queued_qos0_is_not_upgraded(sut: SutHandle) {
    let topic = format!("offline/shared-qos0/{}", unique_client_id("t"));
    if let Some(message) = offline_delivery(&sut, &shared(&topic), &topic, Probe::Qos0).await {
        assert_eq!(
            message.qos, 0,
            "[MQTT-4.8.2-3] QoS 0 message queued for a QoS 1 shared subscription must stay QoS 0"
        );
    }
}

#[conformance_test(
    ids = ["MQTT-4.8.2-3"],
    requires = ["transport.tcp", "shared_subscription_available", "max_qos>=2"],
)]
async fn offline_shared_queued_qos2_is_downgraded(sut: SutHandle) {
    let topic = format!("offline/shared-qos2/{}", unique_client_id("t"));
    let message = offline_delivery(&sut, &shared(&topic), &topic, Probe::Qos2)
        .await
        .expect("QoS 2 message is queued for the offline shared subscriber");
    assert_eq!(
        message.qos, 1,
        "[MQTT-4.8.2-3] QoS 2 message queued for a QoS 1 shared subscription must be delivered at QoS 1"
    );
}

#[conformance_test(
    ids = ["MQTT-3.3.1-12"],
    requires = ["transport.tcp", "shared_subscription_available", "retain_available", "max_qos>=1"],
)]
async fn offline_shared_queued_retain_is_cleared(sut: SutHandle) {
    let topic = format!("offline/shared-retain/{}", unique_client_id("t"));
    let message = offline_delivery(&sut, &shared(&topic), &topic, Probe::Qos1Retained)
        .await
        .expect("retained message is queued for the offline shared subscriber");
    assert!(
        !message.retain,
        "[MQTT-3.3.1-12] Retain As Published 0 must clear RETAIN on queued shared delivery"
    );
}

#[conformance_test(
    ids = ["MQTT-3.3.1-3"],
    requires = ["transport.tcp", "shared_subscription_available", "max_qos>=1"],
)]
async fn offline_shared_queued_dup_is_cleared(sut: SutHandle) {
    let topic = format!("offline/shared-dup/{}", unique_client_id("t"));
    let message = offline_delivery(&sut, &shared(&topic), &topic, Probe::Qos1Dup)
        .await
        .expect("DUP message is queued for the offline shared subscriber");
    assert!(
        !message.dup,
        "[MQTT-3.3.1-3] first delivery of a queued shared message must have DUP 0"
    );
}
