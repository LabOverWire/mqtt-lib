use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5_conformance::harness::unique_client_id;
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use mqtt5_conformance::sut::inprocess_sut_with_config;
use mqtt5_conformance::test_client::TestClient;
use mqtt5_protocol::types::SubscribeOptions;
use mqtt5_protocol::QoS;
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

#[tokio::test]
async fn inbound_receive_maximum_exceeded_disconnects_with_0x93() {
    let config = memory_config().with_server_receive_maximum(2);
    let sut = inprocess_sut_with_config(config).await;

    let mut client = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

#[tokio::test]
async fn qos2_message_not_delivered_before_pubrel() {
    let sut = inprocess_sut_with_config(memory_config()).await;

    let subscriber = TestClient::connect_with_prefix(&sut, "pubrec-nosub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(
            "test/nodelay",
            SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..SubscribeOptions::default()
            },
        )
        .await
        .unwrap();

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("pubrec-raw");
    raw.connect_and_establish(&client_id, Duration::from_secs(10))
        .await;

    raw.send_raw(&RawPacketBuilder::publish_qos2(
        "test/nodelay",
        b"held-back",
        1,
    ))
    .await
    .unwrap();

    raw.expect_pubrec(Duration::from_secs(10))
        .await
        .expect("expected PUBREC from broker");

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        subscription.count(),
        0,
        "this broker holds QoS 2 messages until PUBREL; MQTT v5.0 permits either delivery point, \
         so this is an implementation guarantee rather than a conformance requirement"
    );
}
