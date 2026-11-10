#![cfg(feature = "broker")]

use bytes::BytesMut;
use mqtt5::broker::config::{BrokerConfig, StorageBackend as StorageBackendKind, StorageConfig};
use mqtt5::broker::server::{BrokerShutdownHandle, MqttBroker};
use mqtt5::broker::storage::{DynamicStorage, StorageBackend};
use mqtt5::time::Duration;
use mqtt5::transport::packet_io::read_packet_from_stream;
use mqtt5::types::PublishProperties;
use mqtt5::{MqttClient, PublishOptions, QoS};
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::{MqttPacket, Packet};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use ulid::Ulid;

struct Wire {
    stream: TcpStream,
    buffer: BytesMut,
}

impl Wire {
    async fn send(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.expect("write packet");
    }

    async fn next_within(&mut self, ms: u64) -> Option<Packet> {
        tokio::time::timeout(
            Duration::from_millis(ms),
            read_packet_from_stream(&mut self.stream, 5, &mut self.buffer, 1 << 20),
        )
        .await
        .ok()
        .map(|read| read.expect("read packet"))
    }

    async fn rest(&mut self) -> Vec<String> {
        let mut seen = Vec::new();
        while let Some(packet) = self.next_within(1000).await {
            seen.push(match packet {
                Packet::Publish(publish) => format!(
                    "PUBLISH {} dup={} id={:?}",
                    String::from_utf8_lossy(&publish.payload),
                    publish.dup,
                    publish.packet_id
                ),
                Packet::PubRel(pubrel) => format!("PUBREL id={}", pubrel.packet_id),
                other => format!("{other:?}"),
            });
        }
        seen
    }
}

struct Broker {
    address: String,
    storage: Arc<DynamicStorage>,
    shutdown: BrokerShutdownHandle,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Broker {
    async fn stop(mut self) {
        self.shutdown.shutdown();
        if let Some(handle) = self.handle.take() {
            tokio::time::timeout(Duration::from_secs(10), handle)
                .await
                .expect("broker stops")
                .expect("broker task");
        }
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

async fn started(storage: StorageConfig) -> Broker {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("addr"))
        .with_storage(storage);
    let mut broker = MqttBroker::with_config(config).await.expect("broker");
    let address = broker.local_addr().expect("local addr").to_string();
    let storage = broker.storage().expect("storage");
    let shutdown = broker.shutdown_handle();
    let handle = tokio::spawn(async move {
        if let Err(e) = broker.run().await {
            panic!("broker stopped: {e}");
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    Broker {
        address,
        storage,
        shutdown,
        handle: Some(handle),
    }
}

fn memory() -> StorageConfig {
    StorageConfig {
        backend: StorageBackendKind::Memory,
        enable_persistence: true,
        cleanup_interval: Duration::from_millis(200),
        ..Default::default()
    }
}

fn file(dir: &Path) -> StorageConfig {
    StorageConfig {
        backend: StorageBackendKind::File,
        base_dir: dir.to_path_buf(),
        enable_persistence: true,
        cleanup_interval: Duration::from_millis(200),
        ..Default::default()
    }
}

async fn raw_connect(address: &str, client_id: &str, clean_start: bool, session: u32) -> Wire {
    let mut wire = Wire {
        stream: TcpStream::connect(address).await.expect("connect tcp"),
        buffer: BytesMut::new(),
    };
    let mut connect = Vec::new();
    ConnectPacket::new(
        mqtt5_protocol::types::ConnectOptions::new(client_id)
            .with_clean_start(clean_start)
            .with_session_expiry_interval(session),
    )
    .encode(&mut connect)
    .expect("encode CONNECT");
    wire.send(&connect).await;
    assert!(matches!(
        wire.next_within(5000).await,
        Some(Packet::ConnAck(_))
    ));
    wire
}

async fn subscribed(broker: &Broker, client_id: &str, session: u32, topic: &str, qos: QoS) -> Wire {
    let mut wire = raw_connect(&broker.address, client_id, true, session).await;
    let topic_len = u8::try_from(topic.len()).expect("short topic");
    let mut subscribe = vec![0x82, 6 + topic_len, 0x00, 0x01, 0x00, 0x00, topic_len];
    subscribe.extend_from_slice(topic.as_bytes());
    subscribe.push(qos as u8);
    wire.send(&subscribe).await;
    assert!(matches!(
        wire.next_within(5000).await,
        Some(Packet::SubAck(_))
    ));
    wire
}

async fn publisher(broker: &Broker) -> MqttClient {
    let client = MqttClient::new(format!("expiry-pub-{}", Ulid::new()));
    client
        .connect(&format!("mqtt://{}", broker.address))
        .await
        .expect("publisher");
    client
}

async fn publish(client: &MqttClient, topic: &str, payload: &[u8], qos: QoS, expiry: Option<u32>) {
    client
        .publish_with_options(
            topic,
            payload.to_vec(),
            PublishOptions {
                qos,
                properties: PublishProperties {
                    message_expiry_interval: expiry,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .expect("publish");
}

async fn delivered_then_left(
    broker: &Broker,
    client_id: &str,
    session: u32,
    topic: &str,
    qos: QoS,
    expiry: Option<u32>,
    receives: bool,
) -> MqttClient {
    let mut first = subscribed(broker, client_id, session, topic, qos).await;
    let sender = publisher(broker).await;
    publish(&sender, topic, b"x", qos, expiry).await;
    assert!(matches!(
        first.next_within(5000).await,
        Some(Packet::Publish(_))
    ));
    if receives {
        first.send(&[0x50, 0x02, 0x00, 0x01]).await;
        assert!(matches!(
            first.next_within(5000).await,
            Some(Packet::PubRel(_))
        ));
    }
    sender
}

async fn resumed_after(
    storage: StorageConfig,
    qos: QoS,
    expiry: u32,
    receives: bool,
) -> Vec<String> {
    let broker = started(storage).await;
    let topic = format!("expiry/{}", Ulid::new());
    let client_id = format!("expiry-{}", Ulid::new());
    let sender = delivered_then_left(
        &broker,
        &client_id,
        3600,
        &topic,
        qos,
        Some(expiry),
        receives,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let mut second = raw_connect(&broker.address, &client_id, false, 3600).await;
    publish(&sender, &topic, b"n", qos, None).await;
    second.rest().await
}

#[tokio::test]
async fn an_unacknowledged_qos_1_delivery_is_resent_after_its_message_expiry() {
    assert_eq!(
        resumed_after(memory(), QoS::AtLeastOnce, 1, false).await,
        [
            "PUBLISH x dup=true id=Some(1)",
            "PUBLISH n dup=false id=Some(2)"
        ]
    );
}

#[tokio::test]
async fn an_unacknowledged_qos_2_delivery_is_resent_after_its_message_expiry() {
    assert_eq!(
        resumed_after(memory(), QoS::ExactlyOnce, 1, false).await,
        [
            "PUBLISH x dup=true id=Some(1)",
            "PUBLISH n dup=false id=Some(2)"
        ]
    );
}

#[tokio::test]
async fn a_qos_2_release_is_resent_after_its_message_expiry() {
    assert_eq!(
        resumed_after(memory(), QoS::ExactlyOnce, 1, true).await,
        ["PUBREL id=1", "PUBLISH n dup=false id=Some(2)"]
    );
}

#[tokio::test]
async fn a_qos_2_release_is_resent_after_its_message_expiry_on_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        resumed_after(file(dir.path()), QoS::ExactlyOnce, 1, true).await,
        ["PUBREL id=1", "PUBLISH n dup=false id=Some(2)"]
    );
}

#[tokio::test]
async fn control_an_unacknowledged_delivery_before_its_message_expiry_is_resent() {
    assert_eq!(
        resumed_after(memory(), QoS::AtLeastOnce, 60, false).await,
        [
            "PUBLISH x dup=true id=Some(1)",
            "PUBLISH n dup=false id=Some(2)"
        ]
    );
}

async fn in_flight_left(broker: &Broker, client_id: &str) -> usize {
    assert!(broker
        .storage
        .get_session(client_id)
        .await
        .expect("read session")
        .is_none());
    broker
        .storage
        .get_inflight_messages(client_id)
        .await
        .expect("read in-flight")
        .len()
}

async fn in_flight_left_after_session_expiry(storage: StorageConfig) -> usize {
    let broker = started(storage).await;
    let topic = format!("swept/{}", Ulid::new());
    let client_id = format!("swept-{}", Ulid::new());
    delivered_then_left(&broker, &client_id, 1, &topic, QoS::ExactlyOnce, None, true).await;
    tokio::time::sleep(Duration::from_millis(3500)).await;
    in_flight_left(&broker, &client_id).await
}

#[tokio::test]
async fn session_expiry_clears_a_started_delivery_in_memory() {
    assert_eq!(in_flight_left_after_session_expiry(memory()).await, 0);
}

#[tokio::test]
async fn session_expiry_clears_a_started_delivery_on_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        in_flight_left_after_session_expiry(file(dir.path())).await,
        0
    );
}

async fn in_flight_left_after_expiring_while_down(expiry: Option<u32>) -> usize {
    let dir = tempfile::tempdir().expect("tempdir");
    let topic = format!("down/{}", Ulid::new());
    let client_id = format!("down-{}", Ulid::new());
    let first = started(file(dir.path())).await;
    delivered_then_left(
        &first,
        &client_id,
        1,
        &topic,
        QoS::ExactlyOnce,
        expiry,
        true,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    first.stop().await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let second = started(file(dir.path())).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let left = in_flight_left(&second, &client_id).await;
    second.stop().await;
    left
}

#[tokio::test]
async fn a_session_that_expires_while_the_broker_is_down_leaves_no_started_delivery() {
    assert_eq!(in_flight_left_after_expiring_while_down(Some(1)).await, 0);
}

#[tokio::test]
async fn a_session_that_expires_while_the_broker_is_down_leaves_no_delivery_without_expiry() {
    assert_eq!(in_flight_left_after_expiring_while_down(None).await, 0);
}
