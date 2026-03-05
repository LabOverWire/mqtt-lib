mod common;

use bytes::Bytes;
use common::{MessageCollector, TestBroker};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::events::{BrokerEventHandler, ClientPublishEvent, PublishAction};
use mqtt5::packet::publish::PublishPacket;
use mqtt5::time::Duration;
use mqtt5::PublishOptions;
use mqtt5::{MqttClient, QoS};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

struct SuppressHandler;

impl BrokerEventHandler for SuppressHandler {
    fn on_client_publish<'a>(
        &'a self,
        _event: ClientPublishEvent,
    ) -> Pin<Box<dyn Future<Output = PublishAction> + Send + 'a>> {
        Box::pin(async { PublishAction::Handled })
    }
}

struct TransformHandler {
    new_payload: Bytes,
}

impl BrokerEventHandler for TransformHandler {
    fn on_client_publish<'a>(
        &'a self,
        event: ClientPublishEvent,
    ) -> Pin<Box<dyn Future<Output = PublishAction> + Send + 'a>> {
        let payload = self.new_payload.clone();
        Box::pin(async move {
            let mut packet = PublishPacket::new(event.topic.to_string(), payload, event.qos);
            packet.retain = event.retain;
            packet.packet_id = event.packet_id;
            PublishAction::Transform(packet)
        })
    }
}

async fn start_broker_with_handler(handler: Arc<dyn BrokerEventHandler>) -> TestBroker {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        })
        .with_event_handler(handler);
    TestBroker::start_with_config(config).await
}

async fn connect_client(broker: &TestBroker, name: &str) -> MqttClient {
    let client = MqttClient::new(common::test_client_id(name));
    client
        .connect(broker.address())
        .await
        .expect("connect failed");
    client
}

fn publish_opts(qos: QoS) -> PublishOptions {
    PublishOptions {
        qos,
        ..Default::default()
    }
}

#[tokio::test]
async fn test_suppress_qos0() {
    let broker = start_broker_with_handler(Arc::new(SuppressHandler)).await;

    let collector = MessageCollector::new();
    let sub = connect_client(&broker, "suppress-q0-sub").await;
    sub.subscribe("test/+", collector.callback()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let pub_client = connect_client(&broker, "suppress-q0-pub").await;
    pub_client
        .publish("test/suppress", b"should-not-arrive")
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(collector.count().await, 0);
}

#[tokio::test]
async fn test_suppress_qos1() {
    let broker = start_broker_with_handler(Arc::new(SuppressHandler)).await;

    let collector = MessageCollector::new();
    let sub = connect_client(&broker, "suppress-q1-sub").await;
    sub.subscribe("test/+", collector.callback()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let pub_client = connect_client(&broker, "suppress-q1-pub").await;
    pub_client
        .publish_with_options(
            "test/suppress",
            b"should-not-arrive",
            publish_opts(QoS::AtLeastOnce),
        )
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(collector.count().await, 0);
}

#[tokio::test]
async fn test_suppress_qos2() {
    let broker = start_broker_with_handler(Arc::new(SuppressHandler)).await;

    let collector = MessageCollector::new();
    let sub = connect_client(&broker, "suppress-q2-sub").await;
    sub.subscribe("test/+", collector.callback()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let pub_client = connect_client(&broker, "suppress-q2-pub").await;
    pub_client
        .publish_with_options(
            "test/suppress",
            b"should-not-arrive",
            publish_opts(QoS::ExactlyOnce),
        )
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(collector.count().await, 0);
}

#[tokio::test]
async fn test_transform_qos0() {
    let handler = TransformHandler {
        new_payload: Bytes::from_static(b"transformed"),
    };
    let broker = start_broker_with_handler(Arc::new(handler)).await;

    let collector = MessageCollector::new();
    let sub = connect_client(&broker, "xform-q0-sub").await;
    sub.subscribe("test/+", collector.callback()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let pub_client = connect_client(&broker, "xform-q0-pub").await;
    pub_client
        .publish("test/transform", b"original")
        .await
        .unwrap();

    assert!(
        collector.wait_for_messages(1, Duration::from_secs(2)).await,
        "transformed message should arrive"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].payload, b"transformed");
}

#[tokio::test]
async fn test_transform_qos1() {
    let handler = TransformHandler {
        new_payload: Bytes::from_static(b"transformed-q1"),
    };
    let broker = start_broker_with_handler(Arc::new(handler)).await;

    let collector = MessageCollector::new();
    let sub = connect_client(&broker, "xform-q1-sub").await;
    sub.subscribe("test/+", collector.callback()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let pub_client = connect_client(&broker, "xform-q1-pub").await;
    pub_client
        .publish_with_options(
            "test/transform",
            b"original",
            publish_opts(QoS::AtLeastOnce),
        )
        .await
        .unwrap();

    assert!(
        collector.wait_for_messages(1, Duration::from_secs(2)).await,
        "transformed message should arrive"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].payload, b"transformed-q1");
}

#[tokio::test]
async fn test_transform_qos2() {
    let handler = TransformHandler {
        new_payload: Bytes::from_static(b"transformed-q2"),
    };
    let broker = start_broker_with_handler(Arc::new(handler)).await;

    let collector = MessageCollector::new();
    let sub = connect_client(&broker, "xform-q2-sub").await;
    sub.subscribe("test/+", collector.callback()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let pub_client = connect_client(&broker, "xform-q2-pub").await;
    pub_client
        .publish_with_options(
            "test/transform",
            b"original",
            publish_opts(QoS::ExactlyOnce),
        )
        .await
        .unwrap();

    assert!(
        collector.wait_for_messages(1, Duration::from_secs(2)).await,
        "transformed message should arrive"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].payload, b"transformed-q2");
}

#[tokio::test]
async fn test_continue_preserves_default_behavior() {
    let broker = TestBroker::start().await;

    let collector = MessageCollector::new();
    let sub = connect_client(&broker, "continue-sub").await;
    sub.subscribe("test/+", collector.callback()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let pub_client = connect_client(&broker, "continue-pub").await;
    pub_client
        .publish_with_options("test/continue", b"hello", publish_opts(QoS::AtLeastOnce))
        .await
        .unwrap();

    assert!(
        collector.wait_for_messages(1, Duration::from_secs(2)).await,
        "message should arrive with default Continue action"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].payload, b"hello");
}

#[tokio::test]
async fn test_suppress_multiple_publishes() {
    let broker = start_broker_with_handler(Arc::new(SuppressHandler)).await;

    let collector = MessageCollector::new();
    let sub = connect_client(&broker, "suppress-multi-sub").await;
    sub.subscribe("test/#", collector.callback()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let pub_client = connect_client(&broker, "suppress-multi-pub").await;
    for i in 0..5 {
        pub_client
            .publish_with_options(
                &format!("test/msg{i}"),
                format!("payload-{i}").as_bytes(),
                publish_opts(QoS::AtLeastOnce),
            )
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(collector.count().await, 0);
}
