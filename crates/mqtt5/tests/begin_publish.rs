#![cfg(feature = "broker")]
mod common;

use common::TestBroker;
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::{
    ConnectOptions, Delivery, MqttClient, PublishHandle, PublishOptions, PublishOutcome,
    PublishRejection, QoS,
};
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const PUBLISH_TYPE: u8 = 3;
const SETTLE: Duration = Duration::from_millis(300);

async fn read_packet(stream: &mut OwnedReadHalf) -> Option<(u8, Vec<u8>)> {
    let mut first = [0u8; 1];
    stream.read_exact(&mut first).await.ok()?;
    let mut multiplier = 1usize;
    let mut remaining = 0usize;
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await.ok()?;
        remaining += usize::from(byte[0] & 0x7f) * multiplier;
        if byte[0] & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
    }
    let mut body = vec![0u8; remaining];
    stream.read_exact(&mut body).await.ok()?;
    Some((first[0], body))
}

fn qos1_publish_id_and_payload(body: &[u8]) -> (u16, Vec<u8>) {
    let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
    let id_at = 2 + topic_len;
    let packet_id = u16::from_be_bytes([body[id_at], body[id_at + 1]]);
    let mut cursor = id_at + 2;
    let mut multiplier = 1usize;
    let mut properties_len = 0usize;
    loop {
        let byte = body[cursor];
        cursor += 1;
        properties_len += usize::from(byte & 0x7f) * multiplier;
        if byte & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
    }
    (packet_id, body[cursor + properties_len..].to_vec())
}

fn connack_with_receive_maximum(receive_maximum: u16) -> Vec<u8> {
    let mut connack = ConnAckPacket::new(false, ReasonCode::Success);
    connack.properties.set_receive_maximum(receive_maximum);
    let mut encoded = Vec::new();
    connack.encode(&mut encoded).unwrap();
    encoded
}

type Recorded = Arc<Mutex<Vec<(u16, Vec<u8>)>>>;

struct HeldAckServer {
    addr: SocketAddr,
    published: Recorded,
    acks_sent: Arc<AtomicUsize>,
    allow_acks: mpsc::UnboundedSender<usize>,
}

impl HeldAckServer {
    async fn start(receive_maximum: u16, puback_reason: ReasonCode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let published: Recorded = Arc::new(Mutex::new(Vec::new()));
        let acks_sent = Arc::new(AtomicUsize::new(0));
        let (allow_acks, mut allowances) = mpsc::unbounded_channel::<usize>();
        let recorded = Arc::clone(&published);
        let acked = Arc::clone(&acks_sent);

        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (mut reader, mut writer) = stream.into_split();
            if read_packet(&mut reader).await.is_none() {
                return;
            }
            if writer
                .write_all(&connack_with_receive_maximum(receive_maximum))
                .await
                .is_err()
            {
                return;
            }
            let (ids_tx, mut ids_rx) = mpsc::unbounded_channel::<u16>();
            tokio::spawn(async move {
                while let Some(allowed) = allowances.recv().await {
                    for _ in 0..allowed {
                        let Some(packet_id) = ids_rx.recv().await else {
                            return;
                        };
                        let [high, low] = packet_id.to_be_bytes();
                        let puback = [0x40, 0x03, high, low, u8::from(puback_reason)];
                        if writer.write_all(&puback).await.is_err() {
                            return;
                        }
                        acked.fetch_add(1, Ordering::SeqCst);
                    }
                }
            });
            while let Some((header, body)) = read_packet(&mut reader).await {
                if header >> 4 == PUBLISH_TYPE {
                    let (packet_id, payload) = qos1_publish_id_and_payload(&body);
                    recorded.lock().unwrap().push((packet_id, payload));
                    if ids_tx.send(packet_id).is_err() {
                        return;
                    }
                }
            }
        });

        Self {
            addr,
            published,
            acks_sent,
            allow_acks,
        }
    }

    fn payloads(&self) -> Vec<Vec<u8>> {
        self.published
            .lock()
            .unwrap()
            .iter()
            .map(|(_, payload)| payload.clone())
            .collect()
    }

    fn packet_ids(&self) -> Vec<u16> {
        self.published
            .lock()
            .unwrap()
            .iter()
            .map(|(packet_id, _)| *packet_id)
            .collect()
    }

    fn release(&self, count: usize) {
        self.allow_acks.send(count).unwrap();
    }
}

fn qos1() -> PublishOptions {
    PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    }
}

fn numbered(count: usize) -> Vec<Vec<u8>> {
    (0..count).map(|i| i.to_string().into_bytes()).collect()
}

#[tokio::test]
async fn sequential_begin_publish_keeps_many_in_flight_in_call_order() {
    let server = HeldAckServer::start(100, ReasonCode::Success).await;
    let client = MqttClient::new("begin-publish-in-flight");
    client
        .connect(&format!("mqtt://{}", server.addr))
        .await
        .unwrap();

    let mut handles = Vec::new();
    for payload in numbered(20) {
        let handle =
            tokio::time::timeout(SETTLE, client.begin_publish("t/ordered", payload, qos1()))
                .await
                .expect("begin_publish must return before its PUBACK arrives")
                .unwrap();
        handles.push(handle);
    }
    tokio::time::sleep(SETTLE).await;

    assert_eq!(server.acks_sent.load(Ordering::SeqCst), 0);
    assert!(handles.iter().all(|handle| handle.try_outcome().is_none()));
    assert_eq!(
        server.payloads(),
        numbered(20),
        "all 20 publishes must be on the wire, unacknowledged, in call order"
    );

    let packet_ids = server.packet_ids();
    server.release(20);
    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(
            tokio::time::timeout(Duration::from_secs(5), handle)
                .await
                .unwrap(),
        );
    }
    let expected: Vec<PublishOutcome> = packet_ids
        .into_iter()
        .map(|packet_id| PublishOutcome::Delivered(Delivery::AtLeastOnce { packet_id }))
        .collect();
    assert_eq!(outcomes, expected);
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn begin_publish_waits_for_receive_maximum_in_call_order() {
    let server = HeldAckServer::start(3, ReasonCode::Success).await;
    let client = MqttClient::new("begin-publish-quota");
    client
        .connect(&format!("mqtt://{}", server.addr))
        .await
        .unwrap();

    let returned = Arc::new(AtomicUsize::new(0));
    let publisher = {
        let client = client.clone();
        let returned = Arc::clone(&returned);
        tokio::spawn(async move {
            let mut handles = Vec::new();
            for payload in numbered(6) {
                handles.push(
                    client
                        .begin_publish("t/quota", payload, qos1())
                        .await
                        .unwrap(),
                );
                returned.fetch_add(1, Ordering::SeqCst);
            }
            handles
        })
    };

    tokio::time::sleep(SETTLE).await;
    assert_eq!(returned.load(Ordering::SeqCst), 3);
    assert_eq!(server.payloads(), numbered(3));

    server.release(1);
    tokio::time::sleep(SETTLE).await;
    assert_eq!(returned.load(Ordering::SeqCst), 4);
    assert_eq!(server.payloads(), numbered(4));

    server.release(2);
    let handles: Vec<PublishHandle> = tokio::time::timeout(Duration::from_secs(5), publisher)
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(SETTLE).await;
    assert_eq!(server.payloads(), numbered(6));

    server.release(3);
    for handle in handles {
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), handle)
                .await
                .unwrap(),
            PublishOutcome::Delivered(Delivery::AtLeastOnce { .. })
        ));
    }
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn begin_publish_qos0_handle_is_already_settled() {
    let server = HeldAckServer::start(10, ReasonCode::Success).await;
    let client = MqttClient::new("begin-publish-qos0");
    client
        .connect(&format!("mqtt://{}", server.addr))
        .await
        .unwrap();

    let handle = client
        .begin_publish("t/zero", b"z".to_vec(), PublishOptions::default())
        .await
        .unwrap();
    assert_eq!(
        handle.try_outcome(),
        Some(PublishOutcome::Delivered(Delivery::Unconfirmed))
    );
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn begin_publish_reports_a_refusal_through_the_handle() {
    let server = HeldAckServer::start(10, ReasonCode::NotAuthorized).await;
    let client = MqttClient::new("begin-publish-refused");
    client
        .connect(&format!("mqtt://{}", server.addr))
        .await
        .unwrap();

    let handle = client
        .begin_publish("t/refused", b"r".to_vec(), qos1())
        .await
        .unwrap();
    server.release(1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .unwrap(),
        PublishOutcome::Rejected(PublishRejection::Refused(ReasonCode::NotAuthorized))
    );
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn begin_publish_while_disconnected_queues_and_settles_after_connect() {
    let server = HeldAckServer::start(10, ReasonCode::Success).await;
    let client = MqttClient::with_options(
        ConnectOptions::new("begin-publish-offline")
            .with_clean_start(false)
            .with_session_expiry_interval(3600)
            .with_automatic_reconnect(false),
    );

    let handle = client
        .begin_publish("t/offline", b"q".to_vec(), qos1())
        .await
        .unwrap();
    assert_eq!(handle.try_outcome(), None);

    client
        .connect(&format!("mqtt://{}", server.addr))
        .await
        .unwrap();
    server.release(1);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .unwrap(),
        PublishOutcome::Delivered(Delivery::AtLeastOnce { .. })
    ));
    assert_eq!(server.payloads(), vec![b"q".to_vec()]);
    client.disconnect().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscriber_receives_sequential_begin_publish_in_call_order() {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: false,
            ..Default::default()
        });
    let broker = TestBroker::start_with_config(config).await;

    let subscriber = MqttClient::new("begin-publish-sub");
    subscriber.connect(broker.address()).await.unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    subscriber
        .subscribe_with_options(
            "t/stream",
            mqtt5::SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |message: mqtt5::types::Message| sink.lock().unwrap().push(message.payload),
        )
        .await
        .unwrap();

    let publisher = MqttClient::new("begin-publish-pub");
    publisher.connect(broker.address()).await.unwrap();
    let mut handles = Vec::new();
    for payload in numbered(2000) {
        handles.push(
            publisher
                .begin_publish("t/stream", payload, qos1())
                .await
                .unwrap(),
        );
    }
    for handle in handles {
        assert!(matches!(
            handle.await,
            PublishOutcome::Delivered(Delivery::AtLeastOnce { .. })
        ));
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while received.lock().unwrap().len() < 2000 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(*received.lock().unwrap(), numbered(2000));
    publisher.disconnect().await.unwrap();
    subscriber.disconnect().await.unwrap();
}
