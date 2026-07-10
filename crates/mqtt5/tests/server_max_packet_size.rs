#![cfg(feature = "broker")]
mod common;

use common::{MessageCollector, TestBroker, DEFAULT_TIMEOUT};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::error::MqttError;
use mqtt5::{MqttClient, PublishResult, QoS};
use std::net::SocketAddr;

const BROKER_MAX: usize = 1024;

async fn start_broker_with_max_packet_size(max: usize) -> TestBroker {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        })
        .with_max_packet_size(max);
    TestBroker::start_with_config(config).await
}

async fn assert_oversized_rejected(client: &MqttClient, qos: QoS) {
    let err = client
        .publish_qos("test/big", vec![0u8; 4096], qos)
        .await
        .expect_err("publish exceeding the broker-advertised limit must fail locally");

    match &err {
        MqttError::PacketTooLarge { size, max } => {
            assert!(size > max, "reported size {size} should exceed max {max}");
            assert_eq!(
                *max, BROKER_MAX,
                "enforced max should be the broker's advertised limit for {qos:?}"
            );
        }
        other => panic!("expected PacketTooLarge for {qos:?}, got {other:?}"),
    }

    assert!(
        err.classify().is_none(),
        "PacketTooLarge must classify as non-recoverable so retry loops do not spin"
    );
}

#[tokio::test]
async fn oversized_publish_rejected_for_qos0_qos1_qos2() {
    let broker = start_broker_with_max_packet_size(BROKER_MAX).await;

    let client = MqttClient::new(common::test_client_id("oversized-all-qos"));
    client
        .connect(broker.address())
        .await
        .expect("connect failed");

    for qos in [QoS::AtMostOnce, QoS::AtLeastOnce, QoS::ExactlyOnce] {
        assert_oversized_rejected(&client, qos).await;
    }

    client.disconnect().await.ok();
}

#[tokio::test]
async fn connection_survives_oversized_publish_across_qos() {
    let broker = start_broker_with_max_packet_size(BROKER_MAX).await;

    let client = MqttClient::new(common::test_client_id("survives-oversized"));
    client
        .connect(broker.address())
        .await
        .expect("connect failed");

    for qos in [QoS::AtMostOnce, QoS::AtLeastOnce, QoS::ExactlyOnce] {
        assert_oversized_rejected(&client, qos).await;

        client
            .publish_qos("test/small", vec![0u8; 128], qos)
            .await
            .unwrap_or_else(|e| panic!("normal {qos:?} publish must still succeed on the same connection after a rejected oversized publish, got {e:?}"));
    }

    client.disconnect().await.ok();
}

async fn publish_qos1_packet_id(client: &MqttClient) -> u16 {
    match client
        .publish_qos("test/idcheck", vec![0u8; 32], QoS::AtLeastOnce)
        .await
        .expect("within-limit QoS1 publish should succeed")
    {
        PublishResult::QoS1Or2 { packet_id } => packet_id,
        PublishResult::QoS0 => panic!("expected QoS1Or2 result, got QoS0"),
    }
}

#[tokio::test]
async fn rejected_oversized_publish_does_not_consume_packet_id() {
    let broker = start_broker_with_max_packet_size(BROKER_MAX).await;

    let client = MqttClient::new(common::test_client_id("no-packet-id-leak"));
    client
        .connect(broker.address())
        .await
        .expect("connect failed");

    let first = publish_qos1_packet_id(&client).await;

    for _ in 0..5 {
        assert_oversized_rejected(&client, QoS::AtLeastOnce).await;
    }

    let second = publish_qos1_packet_id(&client).await;
    assert_eq!(
        second,
        first + 1,
        "packet ids must stay contiguous: a size-rejected publish must not consume an id"
    );

    client.disconnect().await.ok();
}

#[tokio::test]
async fn within_limit_payload_round_trips_end_to_end() {
    let broker = start_broker_with_max_packet_size(BROKER_MAX).await;

    let subscriber = MqttClient::new(common::test_client_id("rt-sub"));
    subscriber
        .connect(broker.address())
        .await
        .expect("subscriber connect failed");

    let collector = MessageCollector::new();
    subscriber
        .subscribe("test/roundtrip", collector.callback())
        .await
        .expect("subscribe failed");

    let publisher = MqttClient::new(common::test_client_id("rt-pub"));
    publisher
        .connect(broker.address())
        .await
        .expect("publisher connect failed");

    let payload = vec![7u8; 512];
    publisher
        .publish_qos("test/roundtrip", payload.clone(), QoS::AtLeastOnce)
        .await
        .expect("within-limit publish must succeed");

    assert!(
        collector.wait_for_messages(1, DEFAULT_TIMEOUT).await,
        "subscriber should receive the within-limit message"
    );

    let messages = collector.get_messages().await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].payload, payload, "payload must arrive intact");

    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
}
