#![cfg(feature = "broker")]

use bytes::BytesMut;
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::server::MqttBroker;
use mqtt5::time::Duration;
use mqtt5::transport::packet_io::read_packet_from_stream;
use mqtt5::types::PublishProperties;
use mqtt5::{MqttClient, PublishOptions, QoS};
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::{MqttPacket, Packet};
use mqtt5_protocol::types::ConnectOptions;
use std::net::SocketAddr;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use ulid::Ulid;

const MESSAGES: u32 = 300;

async fn read_next(stream: &mut TcpStream, buffer: &mut BytesMut, ms: u64) -> Option<Packet> {
    tokio::time::timeout(
        Duration::from_millis(ms),
        read_packet_from_stream(stream, 5, buffer, 1 << 20),
    )
    .await
    .ok()
    .map(|read| read.expect("read packet"))
}

async fn delivered_with_expiry(qos: QoS, expiry: u32) -> u32 {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("addr"))
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
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

    let mut stream = TcpStream::connect(&address).await.expect("tcp");
    let mut buffer = BytesMut::new();
    let mut connect = Vec::new();
    ConnectPacket::new(ConnectOptions::new(format!("r364-sub-{}", Ulid::new())))
        .encode(&mut connect)
        .expect("encode");
    stream.write_all(&connect).await.expect("write");
    assert!(matches!(
        read_next(&mut stream, &mut buffer, 5000).await,
        Some(Packet::ConnAck(_))
    ));
    let topic = b"z/t";
    let mut subscribe = vec![0x82, 9, 0x00, 0x01, 0x00, 0x00, 3];
    subscribe.extend_from_slice(topic);
    subscribe.push(qos as u8);
    stream.write_all(&subscribe).await.expect("write");
    assert!(matches!(
        read_next(&mut stream, &mut buffer, 5000).await,
        Some(Packet::SubAck(_))
    ));

    let publisher = MqttClient::new(format!("r364-pub-{}", Ulid::new()));
    publisher
        .connect(&format!("mqtt://{address}"))
        .await
        .expect("publisher");
    for index in 0..MESSAGES {
        publisher
            .publish_with_options(
                "z/t",
                index.to_be_bytes().to_vec(),
                PublishOptions {
                    qos,
                    properties: PublishProperties {
                        message_expiry_interval: Some(expiry),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .expect("publish");
    }

    let mut delivered = 0;
    while let Some(packet) = read_next(&mut stream, &mut buffer, 1500).await {
        if let Packet::Publish(publish) = packet {
            delivered += 1;
            if let Some(packet_id) = publish.packet_id {
                let [high, low] = packet_id.to_be_bytes();
                stream
                    .write_all(&[0x40, 0x02, high, low])
                    .await
                    .expect("puback");
            }
        }
    }
    publisher.disconnect().await.expect("disconnect");
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("broker stops")
        .expect("broker task");
    delivered
}

#[tokio::test]
async fn online_subscriber_gets_every_qos1_message_with_zero_expiry() {
    let delivered = delivered_with_expiry(QoS::AtLeastOnce, 0).await;
    assert_eq!(
        delivered, MESSAGES,
        "QoS 1 messages with Message Expiry Interval 0 delivered to an online subscriber"
    );
}

#[tokio::test]
async fn online_subscriber_gets_every_qos0_message_with_zero_expiry() {
    let delivered = delivered_with_expiry(QoS::AtMostOnce, 0).await;
    assert_eq!(
        delivered, MESSAGES,
        "QoS 0 messages with Message Expiry Interval 0 delivered to an online subscriber"
    );
}

#[tokio::test]
async fn control_online_subscriber_gets_every_qos1_message_with_long_expiry() {
    let delivered = delivered_with_expiry(QoS::AtLeastOnce, 60).await;
    assert_eq!(delivered, MESSAGES);
}

#[tokio::test]
async fn control_online_subscriber_gets_every_qos0_message_with_long_expiry() {
    let delivered = delivered_with_expiry(QoS::AtMostOnce, 60).await;
    assert_eq!(delivered, MESSAGES);
}
