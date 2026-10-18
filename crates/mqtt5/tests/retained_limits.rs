#![cfg(feature = "broker")]
mod common;

use common::{MessageCollector, TestBroker};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::{MqttClient, ProtocolVersion, PublishOptions, QoS};
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::types::{ConnectOptions, WillMessage};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::sleep;

const SIZE_LIMIT: usize = 16;
const SMALL_PAYLOAD: &str = "offline";
const LARGE_PAYLOAD: &str = "a-will-payload-well-over-sixteen-bytes";
const QUOTA_EXCEEDED: u8 = 0x97;
const SUCCESS: u8 = 0x00;
const MARKER: &str = "marker";

async fn broker_with(config: BrokerConfig) -> TestBroker {
    let config = config
        .with_sys_topics_enabled(false)
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address"))
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory));
    TestBroker::start_with_config(config).await
}

fn tcp_addr(broker: &TestBroker) -> &str {
    broker.address().trim_start_matches("mqtt://")
}

async fn connect_with_will(
    broker: &TestBroker,
    client_id: &str,
    will: WillMessage,
    version: ProtocolVersion,
) -> (TcpStream, Option<u8>) {
    let mut stream = TcpStream::connect(tcp_addr(broker))
        .await
        .expect("connect tcp");
    let mut buf = Vec::new();
    let options = ConnectOptions::new(client_id).with_will(will);
    let connect = match version {
        ProtocolVersion::V311 => ConnectPacket::new_v311(options),
        ProtocolVersion::V5 => ConnectPacket::new(options),
    };
    connect.encode(&mut buf).expect("encode CONNECT");
    stream.write_all(&buf).await.expect("write CONNECT");
    let mut connack = [0u8; 64];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut connack))
        .await
        .expect("CONNACK or close in time")
        .unwrap_or(0);
    if read == 0 {
        return (stream, None);
    }
    assert!(read >= 4, "CONNACK complete");
    assert_eq!(connack[0], 0x20, "expected CONNACK");
    (stream, Some(connack[3]))
}

fn will(topic: &str, payload: &str, retain: bool) -> WillMessage {
    WillMessage::new(topic, payload).with_retain(retain)
}

async fn retained_on(broker: &TestBroker, topic: &str) -> usize {
    let client = MqttClient::new(format!("retained-check-{}", topic.replace('/', "-")));
    client
        .connect(broker.address())
        .await
        .expect("check client connects");
    let collector = MessageCollector::new();
    client
        .subscribe(topic, collector.callback())
        .await
        .expect("check client subscribes");
    client.publish(topic, MARKER).await.expect("publish marker");
    for _ in 0..100 {
        let marked = collector
            .get_messages()
            .await
            .iter()
            .any(|message| message.payload == MARKER.as_bytes());
        if marked {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    let retained = collector
        .get_messages()
        .await
        .iter()
        .filter(|message| message.retain)
        .count();
    client.disconnect().await.expect("check client disconnects");
    retained
}

async fn try_publish_retained(broker: &TestBroker, topic: &str) -> bool {
    let client = MqttClient::new("retained-publisher");
    client
        .connect(broker.address())
        .await
        .expect("publisher connects");
    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        retain: true,
        ..PublishOptions::default()
    };
    let accepted = client
        .publish_with_options(topic, SMALL_PAYLOAD, options)
        .await
        .is_ok();
    client.disconnect().await.expect("publisher disconnects");
    accepted
}

async fn publish_retained(broker: &TestBroker, topic: &str) {
    assert!(
        try_publish_retained(broker, topic).await,
        "publish retained"
    );
}

async fn connack_for_will(will: WillMessage, version: ProtocolVersion) -> Option<u8> {
    let broker =
        broker_with(BrokerConfig::default().with_max_retained_message_size(SIZE_LIMIT)).await;
    connect_with_will(&broker, "will-size", will, version)
        .await
        .1
}

#[tokio::test]
async fn retained_will_over_the_size_limit_is_refused_at_connect() {
    assert_eq!(
        connack_for_will(
            will("limits/will", LARGE_PAYLOAD, true),
            ProtocolVersion::V5
        )
        .await,
        Some(QUOTA_EXCEEDED)
    );
}

#[tokio::test]
async fn retained_will_within_the_size_limit_is_accepted() {
    assert_eq!(
        connack_for_will(
            will("limits/will", SMALL_PAYLOAD, true),
            ProtocolVersion::V5
        )
        .await,
        Some(SUCCESS)
    );
}

#[tokio::test]
async fn non_retained_will_over_the_size_limit_is_accepted() {
    assert_eq!(
        connack_for_will(
            will("limits/will", LARGE_PAYLOAD, false),
            ProtocolVersion::V5
        )
        .await,
        Some(SUCCESS)
    );
}

#[tokio::test]
async fn v311_retained_will_over_the_size_limit_closes_without_connack() {
    assert_eq!(
        connack_for_will(
            will("limits/will", LARGE_PAYLOAD, true),
            ProtocolVersion::V311
        )
        .await,
        None
    );
}

#[tokio::test]
async fn v311_retained_will_within_the_size_limit_is_accepted() {
    assert_eq!(
        connack_for_will(
            will("limits/will", SMALL_PAYLOAD, true),
            ProtocolVersion::V311
        )
        .await,
        Some(SUCCESS)
    );
}

#[derive(Debug, PartialEq)]
struct FiredWill {
    delivered_live: bool,
    stored: bool,
}

async fn fire_retained_will(max_retained_messages: usize) -> FiredWill {
    let broker =
        broker_with(BrokerConfig::default().with_max_retained_messages(max_retained_messages))
            .await;
    publish_retained(&broker, "limits/existing").await;

    let watcher = MqttClient::new("will-watcher");
    watcher
        .connect(broker.address())
        .await
        .expect("watcher connects");
    let collector = MessageCollector::new();
    watcher
        .subscribe("limits/will", collector.callback())
        .await
        .expect("watcher subscribes");
    sleep(Duration::from_millis(100)).await;

    let (stream, reason) = connect_with_will(
        &broker,
        "will-count",
        will("limits/will", SMALL_PAYLOAD, true),
        ProtocolVersion::V5,
    )
    .await;
    assert_eq!(reason, Some(SUCCESS), "will client connects");
    drop(stream);

    let delivered_live = collector.wait_for_messages(1, Duration::from_secs(3)).await;
    watcher.disconnect().await.expect("watcher disconnects");
    FiredWill {
        delivered_live,
        stored: retained_on(&broker, "limits/will").await == 1,
    }
}

#[tokio::test]
async fn retained_will_past_the_count_limit_is_delivered_but_not_stored() {
    assert_eq!(
        fire_retained_will(1).await,
        FiredWill {
            delivered_live: true,
            stored: false,
        }
    );
}

#[tokio::test]
async fn retained_will_within_the_count_limit_is_delivered_and_stored() {
    assert_eq!(
        fire_retained_will(2).await,
        FiredWill {
            delivered_live: true,
            stored: true,
        }
    );
}

async fn second_retained_publish_accepted(topic: &str) -> bool {
    let broker = broker_with(BrokerConfig::default().with_max_retained_messages(1)).await;
    publish_retained(&broker, "limits/existing").await;
    try_publish_retained(&broker, topic).await
}

#[tokio::test]
async fn retained_publish_on_a_new_topic_past_the_count_limit_is_refused() {
    assert!(!second_retained_publish_accepted("limits/new").await);
}

#[tokio::test]
async fn retained_publish_replacing_a_stored_topic_at_the_count_limit_is_accepted() {
    assert!(second_retained_publish_accepted("limits/existing").await);
}
