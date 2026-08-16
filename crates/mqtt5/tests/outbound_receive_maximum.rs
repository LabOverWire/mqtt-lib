#![cfg(feature = "broker")]
mod common;

use common::{MessageCollector, TestBroker};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::MqttClient;
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const PUBLISH_TYPE: u8 = 3;

async fn read_one_packet(stream: &mut TcpStream) -> Option<u8> {
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
    Some(first[0] >> 4)
}

fn connack_with_receive_maximum(receive_maximum: u16) -> Vec<u8> {
    let mut connack = ConnAckPacket::new(false, ReasonCode::Success);
    connack.properties.set_receive_maximum(receive_maximum);
    let mut encoded = Vec::new();
    connack.encode(&mut encoded).unwrap();
    encoded
}

/// A stub server that advertises `receive_maximum` in its CONNACK and then
/// withholds all PUBACKs, counting how many PUBLISH packets the client sends.
/// A compliant client self-throttles to the advertised window.
async fn stub_server(
    receive_maximum: u16,
    publish_count: std::sync::Arc<AtomicUsize>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        if read_one_packet(&mut stream).await.is_none() {
            return;
        }
        if stream
            .write_all(&connack_with_receive_maximum(receive_maximum))
            .await
            .is_err()
        {
            return;
        }
        let _ = stream.flush().await;

        while let Some(packet_type) = read_one_packet(&mut stream).await {
            if packet_type == PUBLISH_TYPE {
                publish_count.fetch_add(1, Ordering::SeqCst);
            }
        }
    });

    addr
}

#[tokio::test]
async fn client_self_throttles_to_broker_receive_maximum() {
    let publish_count = std::sync::Arc::new(AtomicUsize::new(0));
    let addr = stub_server(2, publish_count.clone()).await;

    let client = MqttClient::new("outbound-throttle");
    client.connect(&format!("mqtt://{addr}")).await.unwrap();

    let mut handles = Vec::new();
    for i in 0..5u8 {
        let client = client.clone();
        handles.push(tokio::spawn(async move {
            let _ = client
                .publish_qos1("t/throttle", format!("m{i}").into_bytes())
                .await;
        }));
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        publish_count.load(Ordering::SeqCst),
        2,
        "client must self-throttle outbound QoS1 to the broker's advertised Receive Maximum (2)"
    );

    for handle in handles {
        handle.abort();
    }
}

#[tokio::test]
async fn client_rejects_zero_receive_maximum() {
    let publish_count = std::sync::Arc::new(AtomicUsize::new(0));
    let addr = stub_server(0, publish_count).await;

    let client = MqttClient::new("outbound-zero-rm");
    let result = client.connect(&format!("mqtt://{addr}")).await;

    assert!(
        result.is_err(),
        "client must reject a CONNACK advertising a Receive Maximum of 0"
    );
}

fn broker_config(receive_maximum: u16) -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: false,
            ..Default::default()
        })
        .with_server_receive_maximum(receive_maximum)
}

#[tokio::test]
async fn small_window_still_delivers_qos1_and_qos2() {
    let broker = TestBroker::start_with_config(broker_config(2)).await;

    let subscriber = MqttClient::new("small-window-sub");
    subscriber.connect(broker.address()).await.unwrap();
    let collector = MessageCollector::new();
    subscriber
        .subscribe("t/window/#", collector.callback())
        .await
        .unwrap();

    let publisher = MqttClient::new("small-window-pub");
    publisher.connect(broker.address()).await.unwrap();

    for i in 0..10u8 {
        publisher
            .publish_qos1("t/window/one", format!("q1-{i}").into_bytes())
            .await
            .unwrap();
    }
    for i in 0..10u8 {
        publisher
            .publish_qos2("t/window/two", format!("q2-{i}").into_bytes())
            .await
            .unwrap();
    }

    assert!(
        collector
            .wait_for_messages(20, Duration::from_secs(5))
            .await,
        "all QoS1 and QoS2 messages must be delivered despite the window of 2"
    );

    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
    drop(broker);
}
