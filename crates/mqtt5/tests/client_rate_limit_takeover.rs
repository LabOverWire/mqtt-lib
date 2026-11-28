#![cfg(feature = "broker")]
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn rate_limited_broker() -> (u16, tokio::task::JoinHandle<()>) {
    let mut config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory));
    config.max_message_rate_per_client = 1;
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let port = broker.local_addr().unwrap().port();
    let mut ready = broker.ready_receiver();
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&v| v).await.unwrap();
    (port, handle)
}

async fn read_packet(stream: &mut TcpStream) -> Vec<u8> {
    let mut head = [0u8; 2];
    tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut head))
        .await
        .unwrap()
        .unwrap();
    let mut body = vec![0u8; head[1] as usize];
    stream.read_exact(&mut body).await.unwrap();
    let mut packet = head.to_vec();
    packet.extend(body);
    packet
}

async fn connect(port: u16, id: &str) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut body = vec![0, 4, b'M', b'Q', b'T', b'T', 5, 0x02, 0, 60, 0];
    body.extend([0, u8::try_from(id.len()).unwrap()]);
    body.extend(id.as_bytes());
    let mut packet = vec![0x10, u8::try_from(body.len()).unwrap()];
    packet.extend(body);
    stream.write_all(&packet).await.unwrap();
    let connack = read_packet(&mut stream).await;
    assert_eq!(connack[0], 0x20, "{connack:02x?}");
    stream
}

async fn publish_qos1(stream: &mut TcpStream) -> Vec<u8> {
    let publish = [0x32, 0x0A, 0, 4, b'o', b'k', b'/', b'x', 0, 7, 0x00, b'm'];
    stream.write_all(&publish).await.unwrap();
    read_packet(stream).await
}

fn quota_exceeded(ack: &[u8]) -> bool {
    ack.get(4) == Some(&0x97)
}

#[tokio::test]
async fn control_a_single_connection_is_limited() {
    let (port, _broker) = rate_limited_broker().await;
    let mut client = connect(port, "rate-control").await;
    let first = publish_qos1(&mut client).await;
    let second = publish_qos1(&mut client).await;
    assert!(!quota_exceeded(&first), "{first:02x?}");
    assert!(quota_exceeded(&second), "{second:02x?}");
}

#[tokio::test]
async fn a_taken_over_connection_leaves_the_successor_limited() {
    let (port, _broker) = rate_limited_broker().await;
    let displaced = connect(port, "rate-takeover").await;
    let mut successor = connect(port, "rate-takeover").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(displaced);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let first = publish_qos1(&mut successor).await;
    let second = publish_qos1(&mut successor).await;
    assert!(!quota_exceeded(&first), "{first:02x?}");
    assert!(
        quota_exceeded(&second),
        "second publish must get Quota exceeded, got {second:02x?}"
    );
}
