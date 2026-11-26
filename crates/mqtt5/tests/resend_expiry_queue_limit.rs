#![cfg(feature = "broker")]

use bytes::BytesMut;
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::server::{BrokerShutdownHandle, MqttBroker};
use mqtt5::time::Duration;
use mqtt5::transport::packet_io::read_packet_from_stream;
use mqtt5::types::PublishProperties;
use mqtt5::{MqttClient, PublishOptions, QoS};
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::{MqttPacket, Packet};
use mqtt5_protocol::types::ConnectOptions;
use std::net::SocketAddr;
use std::path::Path;
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

    async fn next_publish(&mut self) -> PublishPacket {
        loop {
            match self.next_within(5000).await {
                Some(Packet::Publish(publish)) => return publish,
                Some(_) => {}
                None => panic!("no PUBLISH arrived"),
            }
        }
    }

    async fn publishes(&mut self) -> Vec<PublishPacket> {
        let mut seen = Vec::new();
        while let Some(packet) = self.next_within(1000).await {
            if let Packet::Publish(publish) = packet {
                seen.push(publish);
            }
        }
        seen
    }

    async fn puback(&mut self, packet_id: u16) {
        let [high, low] = packet_id.to_be_bytes();
        self.send(&[0x40, 0x02, high, low]).await;
    }
}

struct Broker {
    address: String,
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

async fn start(storage_config: StorageConfig) -> Broker {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("addr"))
        .with_storage(storage_config);
    let mut broker = MqttBroker::with_config(config).await.expect("broker");
    let address = broker.local_addr().expect("local addr").to_string();
    let shutdown = broker.shutdown_handle();
    let handle = tokio::spawn(async move {
        if let Err(e) = broker.run().await {
            panic!("broker stopped: {e}");
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    Broker {
        address,
        shutdown,
        handle: Some(handle),
    }
}

fn memory(max_queued: usize) -> StorageConfig {
    StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        cleanup_interval: Duration::from_secs(3600),
        max_queued_messages_per_client: max_queued,
        ..Default::default()
    }
}

fn file(dir: &Path, max_queued: usize) -> StorageConfig {
    StorageConfig {
        backend: StorageBackend::File,
        base_dir: dir.to_path_buf(),
        enable_persistence: true,
        cleanup_interval: Duration::from_secs(3600),
        max_queued_messages_per_client: max_queued,
        ..Default::default()
    }
}

async fn raw_connect_with(address: &str, options: ConnectOptions) -> Wire {
    let mut wire = Wire {
        stream: TcpStream::connect(address).await.expect("connect tcp"),
        buffer: BytesMut::new(),
    };
    let mut connect = Vec::new();
    ConnectPacket::new(options)
        .encode(&mut connect)
        .expect("encode CONNECT");
    wire.send(&connect).await;
    assert!(matches!(
        wire.next_within(5000).await,
        Some(Packet::ConnAck(_))
    ));
    wire
}

async fn raw_connect(address: &str, client_id: &str, clean_start: bool) -> Wire {
    raw_connect_with(
        address,
        ConnectOptions::new(client_id)
            .with_clean_start(clean_start)
            .with_session_expiry_interval(3600),
    )
    .await
}

async fn subscribe(wire: &mut Wire, topic: &str) {
    let topic_len = u8::try_from(topic.len()).expect("short topic");
    let mut subscribe = vec![0x82, 6 + topic_len, 0x00, 0x01, 0x00, 0x00, topic_len];
    subscribe.extend_from_slice(topic.as_bytes());
    subscribe.push(QoS::AtLeastOnce as u8);
    wire.send(&subscribe).await;
    assert!(matches!(
        wire.next_within(5000).await,
        Some(Packet::SubAck(_))
    ));
}

async fn publisher(address: &str) -> MqttClient {
    let client = MqttClient::new(format!("rq-pub-{}", Ulid::new()));
    client
        .connect(&format!("mqtt://{address}"))
        .await
        .expect("publisher");
    client
}

async fn publish(client: &MqttClient, topic: &str, payload: &[u8], expiry: Option<u32>) {
    client
        .publish_with_options(
            topic,
            payload.to_vec(),
            PublishOptions {
                qos: QoS::AtLeastOnce,
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

fn resend_interval(received: &[PublishPacket]) -> Option<u32> {
    received
        .iter()
        .find(|publish| publish.dup && publish.payload.as_ref() == b"x")
        .expect("the unacknowledged delivery is resent")
        .properties
        .get_message_expiry_interval()
}

async fn resend_after_holding(broker: &Broker, client_id: &str, held_ms: u64) -> Option<u32> {
    let topic = format!("rq/{}", Ulid::new());
    let mut first = raw_connect(&broker.address, client_id, true).await;
    subscribe(&mut first, &topic).await;
    let sender = publisher(&broker.address).await;
    publish(&sender, &topic, b"x", Some(10)).await;
    let sent = first.next_publish().await;
    assert_eq!(sent.properties.get_message_expiry_interval(), Some(10));
    tokio::time::sleep(Duration::from_millis(held_ms)).await;
    drop(first);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut second = raw_connect(&broker.address, client_id, false).await;
    resend_interval(&second.publishes().await)
}

#[tokio::test]
async fn a_resend_counts_the_time_its_first_delivery_spent_in_flight() {
    let broker = start(memory(10_000)).await;
    let remaining = resend_after_holding(&broker, &format!("rq-{}", Ulid::new()), 4000).await;
    assert!(
        remaining.is_some_and(|seconds| seconds <= 6),
        "resend sent over 4s after a 10s message was received carries {remaining:?}"
    );
}

#[tokio::test]
async fn a_resend_counts_the_time_its_first_delivery_spent_in_flight_on_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let broker = start(file(dir.path(), 10_000)).await;
    let remaining = resend_after_holding(&broker, &format!("rq-{}", Ulid::new()), 4000).await;
    assert!(
        remaining.is_some_and(|seconds| seconds <= 6),
        "resend sent over 4s after a 10s message was received carries {remaining:?}"
    );
}

#[tokio::test]
async fn a_resend_after_a_restart_counts_the_time_since_the_message_was_received() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client_id = format!("rq-{}", Ulid::new());
    let topic = format!("rq/{}", Ulid::new());
    let first_broker = start(file(dir.path(), 10_000)).await;
    let mut first = raw_connect(&first_broker.address, &client_id, true).await;
    subscribe(&mut first, &topic).await;
    let sender = publisher(&first_broker.address).await;
    publish(&sender, &topic, b"x", Some(10)).await;
    first.next_publish().await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    drop(first);
    tokio::time::sleep(Duration::from_millis(300)).await;
    sender.disconnect().await.expect("disconnect publisher");
    first_broker.stop().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let second_broker = start(file(dir.path(), 10_000)).await;
    let mut second = raw_connect(&second_broker.address, &client_id, false).await;
    let remaining = resend_interval(&second.publishes().await);
    drop(second);
    second_broker.stop().await;
    assert!(
        remaining.is_some_and(|seconds| seconds <= 6),
        "resend sent over 4s after a 10s message was received carries {remaining:?}"
    );
}

async fn sent_after_waiting_behind_a_full_window(expiry: u32, wait_ms: u64) -> Vec<PublishPacket> {
    let broker = start(memory(10_000)).await;
    let topic = format!("rq/{}", Ulid::new());
    let mut subscriber = raw_connect_with(
        &broker.address,
        ConnectOptions::new(format!("rq-{}", Ulid::new()))
            .with_clean_start(true)
            .with_session_expiry_interval(3600)
            .with_receive_maximum(1),
    )
    .await;
    subscribe(&mut subscriber, &topic).await;
    let sender = publisher(&broker.address).await;
    publish(&sender, &topic, b"a", None).await;
    let first = subscriber.next_publish().await;
    publish(&sender, &topic, b"w", Some(expiry)).await;
    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
    subscriber
        .puback(first.packet_id.expect("QoS 1 packet id"))
        .await;
    subscriber.publishes().await
}

#[tokio::test]
async fn a_message_waiting_behind_a_full_window_counts_the_time_it_waited() {
    let sent = sent_after_waiting_behind_a_full_window(10, 4000).await;
    let waited = sent
        .iter()
        .find(|publish| publish.payload.as_ref() == b"w")
        .expect("the waiting message is delivered");
    let remaining = waited.properties.get_message_expiry_interval();
    assert!(
        remaining.is_some_and(|seconds| seconds <= 6),
        "message sent over 4s after a 10s message was received carries {remaining:?}"
    );
}

#[tokio::test]
async fn a_message_that_expires_behind_a_full_window_is_not_delivered() {
    let sent = sent_after_waiting_behind_a_full_window(1, 2500).await;
    let payloads: Vec<&[u8]> = sent
        .iter()
        .map(|publish| publish.payload.as_ref())
        .collect();
    assert!(payloads.is_empty(), "received {payloads:?}");
}

async fn delivered_after_an_offline_overflow(storage: StorageConfig) -> Vec<(String, bool)> {
    let broker = start(storage).await;
    let topic = format!("rq/{}", Ulid::new());
    let client_id = format!("rq-{}", Ulid::new());
    let mut first = raw_connect(&broker.address, &client_id, true).await;
    subscribe(&mut first, &topic).await;
    let sender = publisher(&broker.address).await;
    publish(&sender, &topic, b"x", None).await;
    first.next_publish().await;
    drop(first);
    tokio::time::sleep(Duration::from_millis(300)).await;
    publish(&sender, &topic, b"n", None).await;
    let mut second = raw_connect(&broker.address, &client_id, false).await;
    second
        .publishes()
        .await
        .iter()
        .map(|publish| {
            (
                String::from_utf8_lossy(&publish.payload).into_owned(),
                publish.dup,
            )
        })
        .collect()
}

#[tokio::test]
async fn a_full_queue_keeps_a_started_delivery() {
    let seen = delivered_after_an_offline_overflow(memory(1)).await;
    assert_eq!(seen, vec![("x".to_string(), true)], "received {seen:?}");
}

#[tokio::test]
async fn a_full_queue_keeps_a_started_delivery_on_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let seen = delivered_after_an_offline_overflow(file(dir.path(), 1)).await;
    assert_eq!(seen, vec![("x".to_string(), true)], "received {seen:?}");
}

#[tokio::test]
async fn control_a_queue_with_room_delivers_the_resend_then_the_new_message() {
    let seen = delivered_after_an_offline_overflow(memory(2)).await;
    assert_eq!(
        seen,
        vec![("x".to_string(), true), ("n".to_string(), false)],
        "received {seen:?}"
    );
}

#[tokio::test]
async fn a_full_queue_drops_its_oldest_message_not_yet_sent() {
    let broker = start(memory(2)).await;
    let topic = format!("rq/{}", Ulid::new());
    let client_id = format!("rq-{}", Ulid::new());
    let mut first = raw_connect(&broker.address, &client_id, true).await;
    subscribe(&mut first, &topic).await;
    let sender = publisher(&broker.address).await;
    publish(&sender, &topic, b"x", None).await;
    first.next_publish().await;
    drop(first);
    tokio::time::sleep(Duration::from_millis(300)).await;
    for payload in [b"m", b"n"] {
        publish(&sender, &topic, payload, None).await;
    }
    let mut second = raw_connect(&broker.address, &client_id, false).await;
    let seen: Vec<(String, bool)> = second
        .publishes()
        .await
        .iter()
        .map(|publish| {
            (
                String::from_utf8_lossy(&publish.payload).into_owned(),
                publish.dup,
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![("x".to_string(), true), ("n".to_string(), false)],
        "received {seen:?}"
    );
}
