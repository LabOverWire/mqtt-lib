#![cfg(feature = "broker")]

mod common;

use bytes::BytesMut;
use common::{MessageCollector, TestBroker};
use mqtt5::time::Duration;
use mqtt5::transport::packet_io::read_packet_from_stream;
use mqtt5::{ConnectOptions, MqttClient, PublishOptions, QoS, SubscribeOptions};
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::{MqttPacket, Packet};
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

    async fn next(&mut self) -> Packet {
        tokio::time::timeout(
            Duration::from_secs(5),
            read_packet_from_stream(&mut self.stream, 5, &mut self.buffer, 1 << 20),
        )
        .await
        .expect("packet timeout")
        .expect("read packet")
    }
}

async fn previous_process(address: &str, client_id: &str, topic: &str, completes_exchange: bool) {
    let mut wire = Wire {
        stream: TcpStream::connect(address.trim_start_matches("mqtt://"))
            .await
            .expect("connect tcp"),
        buffer: BytesMut::new(),
    };
    let mut connect = Vec::new();
    ConnectPacket::new(
        mqtt5_protocol::types::ConnectOptions::new(client_id)
            .with_clean_start(true)
            .with_session_expiry_interval(3600),
    )
    .encode(&mut connect)
    .expect("encode CONNECT");
    wire.send(&connect).await;
    assert!(matches!(wire.next().await, Packet::ConnAck(_)));

    let payload = b"before-restart";
    let topic_len = u8::try_from(topic.len()).expect("short topic");
    let remaining = 2 + topic_len + 2 + 1 + u8::try_from(payload.len()).expect("short payload");
    let mut publish = vec![0x34, remaining, 0x00, topic_len];
    publish.extend_from_slice(topic.as_bytes());
    publish.extend_from_slice(&[0x00, 0x01, 0x00]);
    publish.extend_from_slice(payload);
    wire.send(&publish).await;
    assert!(matches!(wire.next().await, Packet::PubRec(_)));

    if completes_exchange {
        wire.send(&[0x62, 0x02, 0x00, 0x01]).await;
        assert!(matches!(wire.next().await, Packet::PubComp(_)));
    }
}

async fn payloads_after_restart(completes_exchange: bool) -> Vec<String> {
    let broker = TestBroker::start().await;
    let topic = format!("restart/{}", Ulid::new());
    let collector = MessageCollector::new();
    let subscriber = MqttClient::new(format!("q2-sub-{}", Ulid::new()));
    subscriber
        .connect(broker.address())
        .await
        .expect("subscriber connect");
    subscriber
        .subscribe_with_options(
            &topic,
            SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            collector.callback(),
        )
        .await
        .expect("subscribe");

    let client_id = format!("q2-pub-{}", Ulid::new());
    previous_process(broker.address(), &client_id, &topic, completes_exchange).await;

    let restarted = MqttClient::new(&client_id);
    let connected = restarted
        .connect_with_options(
            broker.address(),
            ConnectOptions::new(&client_id)
                .with_clean_start(false)
                .with_session_expiry_interval(3600)
                .with_resume_existing_session(true),
        )
        .await
        .expect("resume");
    assert!(connected.session_present);
    for payload in ["after-restart-1", "after-restart-2"] {
        restarted
            .publish_with_options(
                &topic,
                payload.as_bytes().to_vec(),
                PublishOptions {
                    qos: QoS::ExactlyOnce,
                    ..Default::default()
                },
            )
            .await
            .expect("publish after restart");
    }

    collector.wait_for_messages(3, Duration::from_secs(3)).await;
    collector
        .get_messages()
        .await
        .into_iter()
        .map(|message| String::from_utf8_lossy(&message.payload).into_owned())
        .collect()
}

#[tokio::test]
async fn completed_exchange_before_restart_delivers_every_publish() {
    let payloads = payloads_after_restart(true).await;
    assert_eq!(
        payloads,
        ["before-restart", "after-restart-1", "after-restart-2"]
    );
}

#[tokio::test]
async fn stranded_qos2_exchange_is_lost_and_its_reused_id_still_delivers() {
    let payloads = payloads_after_restart(false).await;
    assert_eq!(payloads, ["after-restart-1", "after-restart-2"]);
}
