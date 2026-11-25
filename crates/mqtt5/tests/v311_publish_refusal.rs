#![cfg(feature = "broker")]

use mqtt5::broker::acl::{AclManager, AclRule, Permission};
use mqtt5::broker::auth::{ComprehensiveAuthProvider, PasswordAuthProvider};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::{BrokerShutdownHandle, MqttBroker};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const WAIT: Duration = Duration::from_secs(3);
const PACKET_ID: u16 = 7;
const DENIED: &str = "denied/topic";
const ALLOWED: &str = "allowed/topic";
const SMALL: &[u8] = b"ok";
const LARGE: &[u8] = b"a-payload-over-the-limit";
const MAX_PACKET: usize = 128;
const V311: u8 = 4;
const V5: u8 = 5;
const NOT_AUTHORIZED: u8 = 0x87;

type BrokerRun = tokio::task::JoinHandle<mqtt5::Result<()>>;

struct Running {
    addr: SocketAddr,
    shutdown: BrokerShutdownHandle,
    run: BrokerRun,
}

impl Running {
    async fn stop(self) {
        self.shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(5), self.run)
            .await
            .expect("broker stops")
            .expect("broker task joins")
            .expect("broker run succeeds");
    }
}

struct Received {
    header: u8,
    body: [u8; MAX_PACKET],
    len: usize,
}

impl Received {
    fn body(&self) -> &[u8] {
        &self.body[..self.len]
    }
}

async fn start(config: BrokerConfig) -> Running {
    let config = config
        .with_sys_topics_enabled(false)
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_bind_address(([127, 0, 0, 1], 0));
    let provider = Arc::new(ComprehensiveAuthProvider::with_providers(
        PasswordAuthProvider::new().with_anonymous(true),
        AclManager::allow_all(),
    ));
    provider
        .acl_manager()
        .add_rule(AclRule::new(
            "*".to_string(),
            "denied/#".to_string(),
            Permission::Deny,
        ))
        .await;
    let mut broker = MqttBroker::with_config(config)
        .await
        .expect("broker starts")
        .with_auth_provider(provider);
    let addr = broker.local_addr().expect("TCP listener bound");
    let shutdown = broker.shutdown_handle();
    let mut ready = broker.ready_receiver();
    let run = tokio::spawn(async move { broker.run().await });
    ready
        .wait_for(|&is_ready| is_ready)
        .await
        .expect("broker ready");
    Running {
        addr,
        shutdown,
        run,
    }
}

async fn read_packet(stream: &mut TcpStream) -> Option<Received> {
    let mut header = [0u8; 1];
    let read = tokio::time::timeout(WAIT, stream.read(&mut header))
        .await
        .expect("broker answers or closes in time")
        .unwrap_or(0);
    if read == 0 {
        return None;
    }
    let mut len = 0usize;
    let mut shift = 0;
    loop {
        let mut byte = [0u8; 1];
        stream
            .read_exact(&mut byte)
            .await
            .expect("read remaining length");
        len |= usize::from(byte[0] & 0x7f) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    assert!(len <= MAX_PACKET, "packet fits the test buffer");
    let mut body = [0u8; MAX_PACKET];
    stream
        .read_exact(&mut body[..len])
        .await
        .expect("read packet body");
    Some(Received {
        header: header[0],
        body,
        len,
    })
}

async fn connect(addr: SocketAddr, version: u8, client_id: &str) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.expect("tcp connects");
    let id = client_id.as_bytes();
    let id_len = u16::try_from(id.len()).expect("client id length");
    let mut packet = [0u8; MAX_PACKET];
    let mut at = 2;
    for byte in [
        0x00, 0x04, b'M', b'Q', b'T', b'T', version, 0x02, 0x00, 0x3c,
    ] {
        packet[at] = byte;
        at += 1;
    }
    if version == V5 {
        packet[at] = 0x00;
        at += 1;
    }
    packet[at..at + 2].copy_from_slice(&id_len.to_be_bytes());
    at += 2;
    packet[at..at + id.len()].copy_from_slice(id);
    at += id.len();
    packet[0] = 0x10;
    packet[1] = u8::try_from(at - 2).expect("remaining length");
    stream.write_all(&packet[..at]).await.expect("send CONNECT");
    let connack = read_packet(&mut stream).await.expect("CONNACK");
    assert_eq!(connack.header, 0x20, "CONNACK");
    assert_eq!(connack.body()[1], 0x00, "connection accepted");
    stream
}

async fn publish(
    stream: &mut TcpStream,
    version: u8,
    qos: u8,
    retain: bool,
    topic: &str,
    payload: &[u8],
) {
    let topic = topic.as_bytes();
    let topic_len = u16::try_from(topic.len()).expect("topic length");
    let mut packet = [0u8; MAX_PACKET];
    let mut at = 2;
    packet[at..at + 2].copy_from_slice(&topic_len.to_be_bytes());
    at += 2;
    packet[at..at + topic.len()].copy_from_slice(topic);
    at += topic.len();
    if qos > 0 {
        packet[at..at + 2].copy_from_slice(&PACKET_ID.to_be_bytes());
        at += 2;
    }
    if version == V5 {
        packet[at] = 0x00;
        at += 1;
    }
    packet[at..at + payload.len()].copy_from_slice(payload);
    at += payload.len();
    packet[0] = 0x30 | (qos << 1) | u8::from(retain);
    packet[1] = u8::try_from(at - 2).expect("remaining length");
    stream.write_all(&packet[..at]).await.expect("send PUBLISH");
}

async fn assert_accepted(stream: &mut TcpStream, qos: u8) {
    let ack = read_packet(stream).await.expect("acknowledgement");
    let expected = if qos == 1 { 0x40 } else { 0x50 };
    assert_eq!(ack.header, expected, "positive acknowledgement");
    assert_eq!(
        ack.body(),
        PACKET_ID.to_be_bytes(),
        "3.1.1 acknowledgement carries only the packet identifier"
    );
}

async fn assert_closed_without_answer(stream: &mut TcpStream) {
    if let Some(packet) = read_packet(stream).await {
        panic!(
            "3.1.1 client got packet 0x{:02x} with body {:02x?} instead of a closed connection",
            packet.header,
            packet.body()
        );
    }
}

async fn refused_v311(config: BrokerConfig, qos: u8, retain: bool, topic: &str, payload: &[u8]) {
    let running = start(config).await;
    let mut stream = connect(running.addr, V311, "v311-refused").await;
    publish(&mut stream, V311, qos, retain, topic, payload).await;
    assert_closed_without_answer(&mut stream).await;
    running.stop().await;
}

#[tokio::test]
async fn unauthorized_qos1_publish_closes_a_v311_connection() {
    refused_v311(BrokerConfig::default(), 1, false, DENIED, SMALL).await;
}

#[tokio::test]
async fn unauthorized_qos2_publish_closes_a_v311_connection() {
    refused_v311(BrokerConfig::default(), 2, false, DENIED, SMALL).await;
}

#[tokio::test]
async fn qos_over_maximum_closes_a_v311_connection() {
    refused_v311(
        BrokerConfig::default().with_maximum_qos(1),
        2,
        false,
        ALLOWED,
        SMALL,
    )
    .await;
}

#[tokio::test]
async fn oversized_retained_publish_closes_a_v311_connection() {
    refused_v311(
        BrokerConfig::default().with_max_retained_message_size(4),
        1,
        true,
        ALLOWED,
        LARGE,
    )
    .await;
}

#[tokio::test]
async fn retained_publish_over_the_count_limit_closes_a_v311_connection() {
    let running = start(BrokerConfig::default().with_max_retained_messages(1)).await;
    let mut stream = connect(running.addr, V311, "v311-retained-count").await;
    publish(&mut stream, V311, 1, true, "allowed/first", SMALL).await;
    assert_accepted(&mut stream, 1).await;
    publish(&mut stream, V311, 1, true, "allowed/second", SMALL).await;
    assert_closed_without_answer(&mut stream).await;
    running.stop().await;
}

#[tokio::test]
async fn rate_limited_publish_closes_a_v311_connection() {
    let running = start(BrokerConfig {
        max_message_rate_per_client: 1,
        ..BrokerConfig::default()
    })
    .await;
    let mut stream = connect(running.addr, V311, "v311-rate").await;
    publish(&mut stream, V311, 1, false, ALLOWED, SMALL).await;
    assert_accepted(&mut stream, 1).await;
    publish(&mut stream, V311, 1, false, ALLOWED, SMALL).await;
    assert_closed_without_answer(&mut stream).await;
    running.stop().await;
}

#[tokio::test]
async fn receive_maximum_exceeded_closes_a_v311_connection_without_disconnect() {
    let running = start(BrokerConfig::default().with_server_receive_maximum(1)).await;
    let mut stream = connect(running.addr, V311, "v311-receive-max").await;
    publish(&mut stream, V311, 2, false, ALLOWED, SMALL).await;
    assert_accepted(&mut stream, 2).await;
    publish(&mut stream, V311, 2, false, "allowed/other", SMALL).await;
    assert_closed_without_answer(&mut stream).await;
    running.stop().await;
}

#[tokio::test]
async fn unauthorized_qos0_publish_keeps_a_v311_connection_open() {
    let running = start(BrokerConfig::default()).await;
    let mut stream = connect(running.addr, V311, "v311-qos0").await;
    publish(&mut stream, V311, 0, false, DENIED, SMALL).await;
    stream.write_all(&[0xc0, 0x00]).await.expect("send PINGREQ");
    let pingresp = read_packet(&mut stream).await.expect("PINGRESP");
    assert_eq!(pingresp.header, 0xd0, "PINGRESP");
    running.stop().await;
}

#[tokio::test]
async fn session_takeover_closes_a_v311_connection_without_disconnect() {
    let running = start(BrokerConfig::default()).await;
    let mut first = connect(running.addr, V311, "v311-takeover").await;
    let second = connect(running.addr, V311, "v311-takeover").await;
    assert_closed_without_answer(&mut first).await;
    drop(second);
    running.stop().await;
}

#[tokio::test]
async fn unknown_pubrel_gets_a_v311_pubcomp() {
    let running = start(BrokerConfig::default()).await;
    let mut stream = connect(running.addr, V311, "v311-pubrel").await;
    let [high, low] = PACKET_ID.to_be_bytes();
    stream
        .write_all(&[0x62, 0x02, high, low])
        .await
        .expect("send PUBREL");
    let pubcomp = read_packet(&mut stream).await.expect("PUBCOMP");
    assert_eq!(pubcomp.header, 0x70, "PUBCOMP");
    assert_eq!(
        pubcomp.body(),
        PACKET_ID.to_be_bytes(),
        "3.1.1 PUBCOMP carries only the packet identifier"
    );
    running.stop().await;
}

#[tokio::test]
async fn unauthorized_qos1_publish_gets_a_reason_code_on_v5() {
    let running = start(BrokerConfig::default()).await;
    let mut stream = connect(running.addr, V5, "v5-refused").await;
    publish(&mut stream, V5, 1, false, DENIED, SMALL).await;
    let puback = read_packet(&mut stream).await.expect("PUBACK");
    assert_eq!(puback.header, 0x40, "PUBACK");
    assert_eq!(&puback.body()[..2], PACKET_ID.to_be_bytes());
    assert_eq!(puback.body()[2], NOT_AUTHORIZED);
    running.stop().await;
}
