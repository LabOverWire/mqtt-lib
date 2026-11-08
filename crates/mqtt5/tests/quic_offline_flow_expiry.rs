#![cfg(feature = "broker")]
#![cfg(feature = "transport-quic")]

use bytes::BytesMut;
use mqtt5::broker::config::{
    BrokerConfig, QuicConfig as BrokerQuicConfig, StorageBackend, StorageConfig,
};
use mqtt5::broker::{BrokerShutdownHandle, MqttBroker};
use mqtt5::error::Result;
use mqtt5::transport::flow::{DataFlowHeader, FlowFlags, FlowId};
use mqtt5::transport::{QuicConfig, QuicSplitResult, QuicTransport};
use mqtt5::ConnectOptions;
use mqtt5::Transport;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::MqttPacket;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Once;
use std::time::Duration;
use tokio::task::JoinHandle;

const CERT: &str = "../../test_certs/server.pem";
const KEY: &str = "../../test_certs/server.key";
const TOPIC: &str = "flows/expiry";

static CRYPTO_PROVIDER: Once = Once::new();

struct Broker {
    shutdown: BrokerShutdownHandle,
    run: JoinHandle<Result<()>>,
}

impl Broker {
    async fn start() -> (Self, SocketAddr) {
        CRYPTO_PROVIDER.call_once(|| {
            rustls::crypto::ring::default_provider()
                .install_default()
                .expect("install crypto provider");
        });
        let quic = BrokerQuicConfig::new(PathBuf::from(CERT), PathBuf::from(KEY))
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address"));
        let config = BrokerConfig::default()
            .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
            .with_bind_address(([127, 0, 0, 1], 0))
            .with_quic(quic);
        let mut broker = MqttBroker::with_config(config)
            .await
            .expect("broker starts");
        let addr = broker.quic_local_addr().expect("QUIC endpoint bound");
        let shutdown = broker.shutdown_handle();
        let run = tokio::spawn(async move { broker.run().await });
        (Self { shutdown, run }, addr)
    }

    async fn stop(self) {
        self.shutdown.shutdown();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.run).await;
    }
}

fn encode<P: MqttPacket>(packet: &P) -> Vec<u8> {
    let mut bytes = BytesMut::new();
    packet.encode(&mut bytes).expect("encode");
    bytes.to_vec()
}

async fn read_packet(split: &mut QuicSplitResult) -> Vec<u8> {
    let mut header = [0u8; 2];
    split
        .recv
        .read_exact(&mut header)
        .await
        .expect("read header");
    let mut body = vec![0u8; usize::from(header[1])];
    split.recv.read_exact(&mut body).await.expect("read body");
    let mut out = header.to_vec();
    out.extend_from_slice(&body);
    out
}

async fn session(addr: SocketAddr, id: &str, clean: bool) -> (QuicSplitResult, u8) {
    let mut transport =
        QuicTransport::new(QuicConfig::new(addr, "localhost").with_verify_server_cert(false));
    transport.connect().await.expect("QUIC handshake");
    let mut split = transport.into_split().expect("split transport");
    let options = ConnectOptions::new(id)
        .with_clean_start(clean)
        .with_session_expiry_interval(300);
    let connect = ConnectPacket::new(options.protocol_options);
    split
        .send
        .write_all(&encode(&connect))
        .await
        .expect("send CONNECT");
    let connack = read_packet(&mut split).await;
    assert_eq!(connack[0], 0x20, "{connack:?}");
    assert_eq!(connack[3], 0x00, "CONNACK reason {connack:?}");
    (split, connack[2])
}

fn data_flow(sequence: u64, expire: u64) -> Vec<u8> {
    let flags = FlowFlags {
        persistent_subscriptions: 1,
        ..FlowFlags::default()
    };
    let mut buf = BytesMut::new();
    DataFlowHeader::client(FlowId::client(sequence), expire, flags).encode(&mut buf);
    buf.to_vec()
}

fn subscribe_frame(topic: &str) -> Vec<u8> {
    let mut body = vec![0x00, 0x01, 0x00];
    body.extend_from_slice(&u16::try_from(topic.len()).expect("short").to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    body.push(0x00);
    let mut frame = vec![0x82, u8::try_from(body.len()).expect("short")];
    frame.extend_from_slice(&body);
    frame
}

fn publish_frame(topic: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&u16::try_from(topic.len()).expect("short").to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    body.push(0x00);
    body.extend_from_slice(payload);
    let mut frame = vec![0x30, u8::try_from(body.len()).expect("short")];
    frame.extend_from_slice(&body);
    frame
}

async fn subscribe_on(
    split: &mut QuicSplitResult,
    sequence: u64,
    expire: u64,
) -> quinn::SendStream {
    let mut flow = split.connection.open_uni().await.expect("open flow");
    let mut bytes = data_flow(sequence, expire);
    bytes.extend_from_slice(&subscribe_frame(TOPIC));
    flow.write_all(&bytes).await.expect("subscribe on flow");
    let suback = read_packet(split).await;
    assert_eq!(suback, [0x90, 0x04, 0x00, 0x01, 0x00, 0x00]);
    flow
}

async fn delivered(split: &mut QuicSplitResult) -> bool {
    split
        .send
        .write_all(&publish_frame(TOPIC, b"to flow"))
        .await
        .expect("publish");
    matches!(
        tokio::time::timeout(Duration::from_secs(2), split.connection.accept_bi()).await,
        Ok(Ok(_))
    )
}

async fn delivered_after_offline_expiry(second_expire: u64) -> bool {
    let (broker, addr) = Broker::start().await;
    let (mut first, _) = session(addr, "offline-expiry", true).await;
    let long = subscribe_on(&mut first, 5, 300).await;
    let short = subscribe_on(&mut first, 6, second_expire).await;
    first.connection.close(quinn::VarInt::from_u32(0), b"gone");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let (mut second, present) = session(addr, "offline-expiry", false).await;
    assert_eq!(present, 1);
    let got = delivered(&mut second).await;
    broker.stop().await;
    drop((long, short));
    got
}

#[tokio::test]
async fn an_offline_session_keeps_a_filter_both_flows_hold() {
    assert!(delivered_after_offline_expiry(300).await);
}

#[tokio::test]
async fn an_offline_flow_expiry_keeps_a_filter_another_flow_holds() {
    assert!(delivered_after_offline_expiry(1).await);
}
