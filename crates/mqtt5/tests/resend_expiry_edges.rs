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

async fn start() -> Broker {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("addr"))
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            cleanup_interval: Duration::from_secs(3600),
            ..Default::default()
        });
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

async fn raw_connect(address: &str, options: ConnectOptions) -> Wire {
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
    let client = MqttClient::new(format!("rv-pub-{}", Ulid::new()));
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

fn session(client_id: &str, clean_start: bool) -> ConnectOptions {
    ConnectOptions::new(client_id)
        .with_clean_start(clean_start)
        .with_session_expiry_interval(3600)
}

async fn behind_full_window(expiry: u32, wait_ms: u64) -> Vec<(Vec<u8>, Option<u32>)> {
    let broker = start().await;
    let topic = format!("rv/{}", Ulid::new());
    let mut subscriber = raw_connect(
        &broker.address,
        session(&format!("rv-{}", Ulid::new()), true).with_receive_maximum(1),
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
    let seen = subscriber
        .publishes()
        .await
        .into_iter()
        .map(|publish| {
            (
                publish.payload.to_vec(),
                publish.properties.get_message_expiry_interval(),
            )
        })
        .collect();
    sender.disconnect().await.expect("disconnect");
    broker.stop().await;
    seen
}

#[tokio::test]
async fn a_message_whose_interval_passed_by_600ms_behind_a_full_window_is_not_delivered() {
    let seen = behind_full_window(1, 1600).await;
    assert!(
        seen.is_empty(),
        "1s message released 1.6s after it was received: {seen:?}"
    );
}

#[tokio::test]
async fn control_a_message_behind_a_full_window_with_time_left_is_delivered() {
    let seen = behind_full_window(3, 1600).await;
    assert_eq!(seen.len(), 1, "received {seen:?}");
    assert_eq!(seen[0].0, b"w");
    assert!(
        matches!(seen[0].1, Some(1 | 2)),
        "3s message released 1.6s after it was received: {seen:?}"
    );
}

async fn resends_after_two_holds(
    expiry: u32,
    first_hold_ms: u64,
    second_hold_ms: u64,
) -> [Option<u32>; 2] {
    let broker = start().await;
    let client_id = format!("rv-{}", Ulid::new());
    let topic = format!("rv/{}", Ulid::new());
    let mut wire = raw_connect(&broker.address, session(&client_id, true)).await;
    subscribe(&mut wire, &topic).await;
    let sender = publisher(&broker.address).await;
    publish(&sender, &topic, b"x", Some(expiry)).await;
    let sent = wire.next_publish().await;
    assert_eq!(sent.properties.get_message_expiry_interval(), Some(expiry));
    let mut intervals = [None; 2];
    for (slot, hold) in intervals.iter_mut().zip([first_hold_ms, second_hold_ms]) {
        tokio::time::sleep(Duration::from_millis(hold)).await;
        drop(wire);
        tokio::time::sleep(Duration::from_millis(200)).await;
        wire = raw_connect(&broker.address, session(&client_id, false)).await;
        let resend = wire
            .publishes()
            .await
            .into_iter()
            .find(|publish| publish.dup && publish.payload.as_ref() == b"x")
            .expect("the unacknowledged delivery is resent");
        *slot = resend.properties.get_message_expiry_interval();
    }
    drop(wire);
    sender.disconnect().await.expect("disconnect");
    broker.stop().await;
    intervals
}

#[tokio::test]
async fn a_resend_whose_interval_passed_carries_zero() {
    let [first, second] = resends_after_two_holds(2, 2500, 1500).await;
    assert_eq!(first, Some(0), "resend 2.7s into a 2s interval");
    assert_eq!(
        second,
        Some(0),
        "second resend about 4.5s into a 2s interval"
    );
}

#[tokio::test]
async fn two_resends_count_the_whole_time_once() {
    let [first, second] = resends_after_two_holds(20, 2500, 2500).await;
    assert!(
        matches!(first, Some(17 | 18)),
        "resend about 2.7s into a 20s interval carries {first:?}"
    );
    assert!(
        matches!(second, Some(13 | 14)),
        "second resend about 6.5s into a 20s interval carries {second:?}"
    );
}
