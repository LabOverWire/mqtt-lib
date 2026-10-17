#![cfg(feature = "broker")]

use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::BrokerShutdownHandle;
use mqtt5::broker::MqttBroker;
use mqtt5::{
    ConnectOptions, MqttClient, ProtocolVersion, PublishOptions, PublishProperties, QoS,
    SubscribeOptions,
};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const TOPIC: &str = "v311/delivery";
const PAYLOAD: &[u8] = b"{\"x\":5}";
const MAX_PACKET: usize = 256;
const WAIT: Duration = Duration::from_secs(3);
const SUBSCRIPTION_ID: u32 = 7;

#[derive(Clone, Copy)]
enum Delivery {
    Live,
    Retained,
    Queued,
}

async fn publish_v5(addr: SocketAddr, retain: bool) {
    let client = MqttClient::new("v5-publisher");
    client
        .connect(&format!("mqtt://{addr}"))
        .await
        .expect("publisher connects");
    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        retain,
        properties: PublishProperties {
            user_properties: vec![("origin".to_string(), "v5".to_string())],
            ..PublishProperties::default()
        },
        ..PublishOptions::default()
    };
    client
        .publish_with_options(TOPIC, PAYLOAD, options)
        .await
        .expect("publish");
    client.disconnect().await.expect("publisher disconnects");
}

async fn connect_v311(addr: SocketAddr, clean_session: bool) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.expect("tcp connects");
    let client_id = b"v311-subscriber";
    let flags = if clean_session { 0x02 } else { 0x00 };
    let mut connect = [0u8; 2 + 10 + 2 + 15];
    connect[0] = 0x10;
    connect[1] = u8::try_from(connect.len() - 2).expect("remaining length");
    connect[2..12].copy_from_slice(&[0x00, 0x04, b'M', b'Q', b'T', b'T', 0x04, flags, 0x00, 0x3c]);
    connect[12..14].copy_from_slice(&[0x00, 0x0f]);
    connect[14..].copy_from_slice(client_id);
    stream.write_all(&connect).await.expect("send CONNECT");
    let mut connack = [0u8; 4];
    tokio::time::timeout(WAIT, stream.read_exact(&mut connack))
        .await
        .expect("CONNACK in time")
        .expect("read CONNACK");
    assert_eq!(connack[0], 0x20, "CONNACK");
    assert_eq!(connack[3], 0x00, "connection accepted");
    stream
}

async fn subscribe_v311(stream: &mut TcpStream) {
    let topic = TOPIC.as_bytes();
    let mut subscribe = [0u8; 2 + 2 + 2 + TOPIC.len() + 1];
    subscribe[0] = 0x82;
    subscribe[1] = u8::try_from(subscribe.len() - 2).expect("remaining length");
    subscribe[2..4].copy_from_slice(&[0x00, 0x01]);
    subscribe[4..6].copy_from_slice(
        &u16::try_from(topic.len())
            .expect("topic length")
            .to_be_bytes(),
    );
    subscribe[6..6 + topic.len()].copy_from_slice(topic);
    subscribe[6 + topic.len()] = 0x01;
    stream.write_all(&subscribe).await.expect("send SUBSCRIBE");
    let mut suback = [0u8; 5];
    tokio::time::timeout(WAIT, stream.read_exact(&mut suback))
        .await
        .expect("SUBACK in time")
        .expect("read SUBACK");
    assert_eq!(
        suback,
        [0x90, 0x03, 0x00, 0x01, 0x01],
        "SUBACK granting QoS 1"
    );
}

async fn received_payload(stream: &mut TcpStream) -> ([u8; MAX_PACKET], usize) {
    let mut header = [0u8; 1];
    tokio::time::timeout(WAIT, stream.read_exact(&mut header))
        .await
        .expect("PUBLISH in time")
        .expect("read PUBLISH header");
    assert_eq!(header[0] & 0xf0, 0x30, "PUBLISH");
    let mut remaining = 0usize;
    let mut shift = 0;
    loop {
        let mut byte = [0u8; 1];
        stream
            .read_exact(&mut byte)
            .await
            .expect("read remaining length");
        remaining |= usize::from(byte[0] & 0x7f) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    assert!(remaining <= MAX_PACKET, "PUBLISH fits the test buffer");
    let mut body = [0u8; MAX_PACKET];
    stream
        .read_exact(&mut body[..remaining])
        .await
        .expect("read PUBLISH body");
    let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
    let mut start = 2 + topic_len;
    if header[0] & 0x06 != 0 {
        stream
            .write_all(&[0x40, 0x02, body[start], body[start + 1]])
            .await
            .expect("send PUBACK");
        start += 2;
    }
    let mut payload = [0u8; MAX_PACKET];
    let len = remaining - start;
    payload[..len].copy_from_slice(&body[start..remaining]);
    (payload, len)
}

type BrokerRun = tokio::task::JoinHandle<mqtt5::Result<()>>;

async fn start_broker() -> (SocketAddr, BrokerShutdownHandle, BrokerRun) {
    let config = BrokerConfig::default()
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_bind_address(([127, 0, 0, 1], 0));
    let mut broker = MqttBroker::with_config(config)
        .await
        .expect("broker starts");
    let addr = broker.local_addr().expect("TCP listener bound");
    let shutdown = broker.shutdown_handle();
    let run = tokio::spawn(async move { broker.run().await });
    (addr, shutdown, run)
}

async fn stop_broker(shutdown: BrokerShutdownHandle, run: BrokerRun) {
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("broker stops")
        .expect("broker task joins")
        .expect("broker run succeeds");
}

async fn delivered_to_v311(delivery: Delivery) -> ([u8; MAX_PACKET], usize) {
    let (addr, shutdown, run) = start_broker().await;
    let received = match delivery {
        Delivery::Live => {
            let mut subscriber = connect_v311(addr, true).await;
            subscribe_v311(&mut subscriber).await;
            publish_v5(addr, false).await;
            received_payload(&mut subscriber).await
        }
        Delivery::Retained => {
            publish_v5(addr, true).await;
            let mut subscriber = connect_v311(addr, true).await;
            subscribe_v311(&mut subscriber).await;
            received_payload(&mut subscriber).await
        }
        Delivery::Queued => {
            let mut subscriber = connect_v311(addr, false).await;
            subscribe_v311(&mut subscriber).await;
            subscriber
                .write_all(&[0xe0, 0x00])
                .await
                .expect("send DISCONNECT");
            drop(subscriber);
            tokio::time::sleep(Duration::from_millis(200)).await;
            publish_v5(addr, false).await;
            let mut subscriber = connect_v311(addr, false).await;
            received_payload(&mut subscriber).await
        }
    };
    stop_broker(shutdown, run).await;
    received
}

async fn publish_v311(addr: SocketAddr, retain: bool) {
    let options =
        ConnectOptions::new("v311-publisher").with_protocol_version(ProtocolVersion::V311);
    let client = MqttClient::with_options(options.clone());
    Box::pin(client.connect_with_options(&format!("mqtt://{addr}"), options))
        .await
        .expect("publisher connects");
    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        retain,
        ..PublishOptions::default()
    };
    client
        .publish_with_options(TOPIC, PAYLOAD, options)
        .await
        .expect("publish");
    client.disconnect().await.expect("publisher disconnects");
}

async fn subscription_ids_at_v5_subscriber(retained: bool) -> Vec<u32> {
    let (addr, shutdown, run) = start_broker().await;
    if retained {
        publish_v311(addr, true).await;
    }
    let subscriber = MqttClient::new("v5-subscriber");
    subscriber
        .connect(&format!("mqtt://{addr}"))
        .await
        .expect("subscriber connects");
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let options = SubscribeOptions {
        qos: QoS::AtLeastOnce,
        ..SubscribeOptions::default()
    }
    .with_subscription_identifier(SUBSCRIPTION_ID);
    subscriber
        .subscribe_with_options(TOPIC, options, move |message| {
            tx.try_send(message.properties.subscription_identifiers)
                .ok();
        })
        .await
        .expect("subscribe");
    if !retained {
        publish_v311(addr, false).await;
    }
    let ids = tokio::time::timeout(WAIT, rx.recv())
        .await
        .expect("message in time")
        .expect("message received");
    subscriber
        .disconnect()
        .await
        .expect("subscriber disconnects");
    stop_broker(shutdown, run).await;
    ids
}

async fn assert_payload_unchanged(delivery: Delivery) {
    let (payload, len) = delivered_to_v311(delivery).await;
    assert_eq!(&payload[..len], PAYLOAD);
}

#[tokio::test]
async fn live_delivery_to_v311_subscriber_carries_the_published_payload() {
    assert_payload_unchanged(Delivery::Live).await;
}

#[tokio::test]
async fn retained_delivery_to_v311_subscriber_carries_the_published_payload() {
    assert_payload_unchanged(Delivery::Retained).await;
}

#[tokio::test]
async fn queued_delivery_to_v311_subscriber_carries_the_published_payload() {
    assert_payload_unchanged(Delivery::Queued).await;
}

#[tokio::test]
async fn live_delivery_from_v311_publisher_carries_the_subscription_identifier() {
    assert_eq!(
        subscription_ids_at_v5_subscriber(false).await,
        [SUBSCRIPTION_ID]
    );
}

#[tokio::test]
async fn retained_delivery_from_v311_publisher_carries_the_subscription_identifier() {
    assert_eq!(
        subscription_ids_at_v5_subscriber(true).await,
        [SUBSCRIPTION_ID]
    );
}
