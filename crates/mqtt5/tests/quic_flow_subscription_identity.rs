#![cfg(feature = "broker")]
#![cfg(feature = "transport-quic")]

use bytes::BytesMut;
use mqtt5::broker::config::{
    BrokerConfig, QuicConfig as BrokerQuicConfig, StorageBackend, StorageConfig,
};
use mqtt5::broker::router::MessageRouter;
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
use std::sync::{Arc, Once};
use std::time::Duration;
use tokio::task::JoinHandle;

const CERT: &str = "../../test_certs/server.pem";
const KEY: &str = "../../test_certs/server.key";
const TOPIC: &str = "flows/identity";
const GRANTED: u8 = 0x00;
const QUOTA_EXCEEDED: u8 = 0x97;

static CRYPTO_PROVIDER: Once = Once::new();

struct Broker {
    shutdown: BrokerShutdownHandle,
    run: JoinHandle<Result<()>>,
    router: Arc<MessageRouter>,
}

impl Broker {
    async fn start(max_subscriptions: usize) -> (Self, SocketAddr) {
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
            .with_max_subscriptions_per_client(max_subscriptions)
            .with_quic(quic);
        let mut broker = MqttBroker::with_config(config)
            .await
            .expect("broker starts");
        let addr = broker.quic_local_addr().expect("QUIC endpoint bound");
        let shutdown = broker.shutdown_handle();
        let router = broker.router();
        let run = tokio::spawn(async move { broker.run().await });
        (
            Self {
                shutdown,
                run,
                router,
            },
            addr,
        )
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

fn data_flow(sequence: u64) -> Vec<u8> {
    let flags = FlowFlags {
        persistent_subscriptions: 1,
        ..FlowFlags::default()
    };
    let mut buf = BytesMut::new();
    DataFlowHeader::client(FlowId::client(sequence), 300, flags).encode(&mut buf);
    buf.to_vec()
}

fn filter_frame(packet_type: u8, packet_id: u16, with_options: bool) -> Vec<u8> {
    let mut body = packet_id.to_be_bytes().to_vec();
    body.push(0x00);
    body.extend_from_slice(&u16::try_from(TOPIC.len()).expect("short").to_be_bytes());
    body.extend_from_slice(TOPIC.as_bytes());
    if with_options {
        body.push(0x00);
    }
    let mut frame = vec![packet_type, u8::try_from(body.len()).expect("short")];
    frame.extend_from_slice(&body);
    frame
}

fn publish_frame(payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&u16::try_from(TOPIC.len()).expect("short").to_be_bytes());
    body.extend_from_slice(TOPIC.as_bytes());
    body.push(0x00);
    body.extend_from_slice(payload);
    let mut frame = vec![0x30, u8::try_from(body.len()).expect("short")];
    frame.extend_from_slice(&body);
    frame
}

async fn on_flow(
    split: &mut QuicSplitResult,
    sequence: u64,
    frame: Vec<u8>,
) -> (quinn::SendStream, Vec<u8>) {
    let mut flow = split.connection.open_uni().await.expect("open flow");
    let mut bytes = data_flow(sequence);
    bytes.extend_from_slice(&frame);
    flow.write_all(&bytes).await.expect("write on flow");
    let ack = read_packet(split).await;
    (flow, ack)
}

async fn subscribe_on(
    split: &mut QuicSplitResult,
    sequence: u64,
    packet_id: u16,
) -> (quinn::SendStream, u8) {
    let (flow, suback) = on_flow(split, sequence, filter_frame(0x82, packet_id, true)).await;
    assert_eq!(suback[0], 0x90, "{suback:?}");
    assert_eq!(&suback[2..4], &packet_id.to_be_bytes(), "{suback:?}");
    let reason = *suback.last().expect("SUBACK reason code");
    (flow, reason)
}

async fn unsubscribe_on(
    split: &mut QuicSplitResult,
    sequence: u64,
    packet_id: u16,
) -> quinn::SendStream {
    let (flow, unsuback) = on_flow(split, sequence, filter_frame(0xA2, packet_id, false)).await;
    assert_eq!(
        unsuback,
        [
            0xB0,
            0x04,
            packet_id.to_be_bytes()[0],
            packet_id.to_be_bytes()[1],
            0x00,
            0x00
        ]
    );
    flow
}

async fn delivered(split: &mut QuicSplitResult) -> bool {
    split
        .send
        .write_all(&publish_frame(b"to flow"))
        .await
        .expect("publish");
    matches!(
        tokio::time::timeout(Duration::from_secs(2), split.connection.accept_bi()).await,
        Ok(Ok(_))
    )
}

#[tokio::test]
async fn the_quota_counts_a_filter_subscribed_on_a_new_flow() {
    let (broker, addr) = Broker::start(1).await;
    let (mut client, _) = session(addr, "quota-new-flow", true).await;
    let (first, first_reason) = subscribe_on(&mut client, 5, 1).await;
    let (second, second_reason) = subscribe_on(&mut client, 6, 2).await;
    let count = broker
        .router
        .subscription_count_for_client("quota-new-flow")
        .await;
    broker.stop().await;
    drop((first, second));
    assert_eq!(first_reason, GRANTED);
    assert_eq!(second_reason, QUOTA_EXCEEDED);
    assert_eq!(count, 1);
}

#[tokio::test]
async fn the_quota_lets_a_filter_be_subscribed_again_on_its_flow() {
    let (broker, addr) = Broker::start(1).await;
    let (mut client, _) = session(addr, "quota-same-flow", true).await;
    let (first, first_reason) = subscribe_on(&mut client, 5, 1).await;
    let (again, again_reason) = subscribe_on(&mut client, 5, 2).await;
    let count = broker
        .router
        .subscription_count_for_client("quota-same-flow")
        .await;
    broker.stop().await;
    drop((first, again));
    assert_eq!(first_reason, GRANTED);
    assert_eq!(again_reason, GRANTED);
    assert_eq!(count, 1);
}

#[tokio::test]
async fn a_resumed_session_restores_the_filter_on_every_flow() {
    let (broker, addr) = Broker::start(0).await;
    let (mut first, _) = session(addr, "resume-flows", true).await;
    let (five, five_reason) = subscribe_on(&mut first, 5, 1).await;
    let (six, six_reason) = subscribe_on(&mut first, 6, 2).await;
    assert_eq!((five_reason, six_reason), (GRANTED, GRANTED));
    first.connection.close(quinn::VarInt::from_u32(0), b"gone");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (mut second, present) = session(addr, "resume-flows", false).await;
    assert_eq!(present, 1);
    let restored = broker
        .router
        .subscription_count_for_client("resume-flows")
        .await;
    let reopened = unsubscribe_on(&mut second, 6, 3).await;
    let got = delivered(&mut second).await;
    broker.stop().await;
    drop((five, six, reopened));
    assert!(
        got,
        "the subscription on flow 5 was lost when the session resumed"
    );
    assert_eq!(restored, 2);
}

#[tokio::test]
async fn a_resumed_session_keeps_the_flow_left_after_an_unsubscribe() {
    let (broker, addr) = Broker::start(0).await;
    let (mut first, _) = session(addr, "resume-after-unsubscribe", true).await;
    let (five, five_reason) = subscribe_on(&mut first, 5, 1).await;
    let (six, six_reason) = subscribe_on(&mut first, 6, 2).await;
    assert_eq!((five_reason, six_reason), (GRANTED, GRANTED));
    let unsubscribed = unsubscribe_on(&mut first, 5, 3).await;
    first.connection.close(quinn::VarInt::from_u32(0), b"gone");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (mut second, present) = session(addr, "resume-after-unsubscribe", false).await;
    assert_eq!(present, 1);
    let restored = broker
        .router
        .subscription_count_for_client("resume-after-unsubscribe")
        .await;
    let got = delivered(&mut second).await;
    broker.stop().await;
    drop((five, six, unsubscribed));
    assert_eq!(restored, 1);
    assert!(got);
}
