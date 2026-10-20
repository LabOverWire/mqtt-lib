#![cfg(feature = "broker")]
#![cfg(feature = "transport-quic")]

use bytes::BytesMut;
use mqtt5::broker::config::{
    BrokerConfig, QuicConfig as BrokerQuicConfig, ServerDeliveryStrategy, StorageBackend,
    StorageConfig,
};
use mqtt5::broker::{BrokerShutdownHandle, MqttBroker};
use mqtt5::error::Result;
use mqtt5::transport::flow::{ControlFlowHeader, DataFlowHeader, FlowFlags, FlowId};
use mqtt5::transport::{QuicConfig, QuicSplitResult, QuicTransport};
use mqtt5::Transport;
use mqtt5_protocol::{QuicConnectionCode, QuicStreamCode};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Once;
use std::time::Duration;
use tokio::task::JoinHandle;

const CERT: &str = "../../test_certs/server.pem";
const KEY: &str = "../../test_certs/server.key";
const PINGREQ: [u8; 2] = [0xC0, 0x00];
const USER_DEFINED_FLOWS: [&[u8]; 2] = [&[0x14, 0x02, b'h', b'i'], &[0x14, 0x61, 0x70, 0x70]];

#[derive(Debug, PartialEq)]
struct Outcome {
    session_alive: bool,
    close_code: Option<u64>,
}

const STILL_SERVED: Outcome = Outcome {
    session_alive: true,
    close_code: None,
};

static CRYPTO_PROVIDER: Once = Once::new();

struct Broker {
    shutdown: BrokerShutdownHandle,
    run: JoinHandle<Result<()>>,
}

impl Broker {
    async fn start() -> (Self, SocketAddr) {
        let defaults = BrokerConfig::default();
        Self::start_with(defaults.max_packet_size, defaults.server_delivery_strategy).await
    }

    async fn start_with(
        max_packet_size: usize,
        delivery: ServerDeliveryStrategy,
    ) -> (Self, SocketAddr) {
        CRYPTO_PROVIDER.call_once(|| {
            rustls::crypto::ring::default_provider()
                .install_default()
                .expect("install crypto provider");
        });
        let config = BrokerConfig::default()
            .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
            .with_bind_address(([127, 0, 0, 1], 0))
            .with_max_packet_size(max_packet_size)
            .with_server_delivery_strategy(delivery)
            .with_quic(
                BrokerQuicConfig::new(PathBuf::from(CERT), PathBuf::from(KEY))
                    .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address")),
            );
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
        tokio::time::timeout(Duration::from_secs(5), self.run)
            .await
            .expect("broker stops")
            .expect("broker task joins")
            .expect("broker run succeeds");
    }
}

fn connect_frame(client_id: &str) -> Vec<u8> {
    let mut body = vec![
        0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05, 0x02, 0x00, 0x3C, 0x00,
    ];
    body.extend_from_slice(
        &u16::try_from(client_id.len())
            .expect("client id fits u16")
            .to_be_bytes(),
    );
    body.extend_from_slice(client_id.as_bytes());
    let mut frame = vec![0x10, u8::try_from(body.len()).expect("short CONNECT")];
    frame.extend_from_slice(&body);
    frame
}

async fn connected_session(client_id: &str) -> (Broker, QuicSplitResult) {
    let (broker, addr) = Broker::start().await;
    (broker, session_at(addr, client_id).await)
}

async fn session_at(addr: SocketAddr, client_id: &str) -> QuicSplitResult {
    let mut transport =
        QuicTransport::new(QuicConfig::new(addr, "localhost").with_verify_server_cert(false));
    transport.connect().await.expect("QUIC handshake");
    let mut split = transport.into_split().expect("split transport");
    split
        .send
        .write_all(&connect_frame(client_id))
        .await
        .expect("send CONNECT");
    let mut header = [0u8; 2];
    split
        .recv
        .read_exact(&mut header)
        .await
        .expect("read CONNACK header");
    assert_eq!(header[0], 0x20);
    let mut body = vec![0u8; usize::from(header[1])];
    split
        .recv
        .read_exact(&mut body)
        .await
        .expect("read CONNACK body");
    split
}

async fn outcome(split: &mut QuicSplitResult) -> Outcome {
    let mut response = [0u8; 2];
    let session_alive = split.send.write_all(&PINGREQ).await.is_ok()
        && matches!(
            tokio::time::timeout(Duration::from_secs(2), split.recv.read_exact(&mut response))
                .await,
            Ok(Ok(()))
        )
        && response == [0xD0, 0x00];
    let close_code = tokio::time::timeout(Duration::from_secs(3), split.connection.closed())
        .await
        .ok()
        .and_then(|error| match error {
            quinn::ConnectionError::ApplicationClosed(close) => Some(close.error_code.into_inner()),
            _ => None,
        });
    Outcome {
        session_alive,
        close_code,
    }
}

#[derive(Debug, PartialEq)]
enum StreamEnd {
    Stopped(u64),
    Refused(u64),
    Finished,
    Open,
}

fn code(code: QuicStreamCode) -> u64 {
    u64::from(code.code())
}

fn data_flow(sequence: u64, expire: u64, flags: FlowFlags) -> Vec<u8> {
    let mut buf = BytesMut::new();
    DataFlowHeader::client(FlowId::client(sequence), expire, flags).encode(&mut buf);
    buf.to_vec()
}

fn with_pingreq(mut header: Vec<u8>) -> Vec<u8> {
    header.extend_from_slice(&PINGREQ);
    header
}

fn server_flow() -> Vec<u8> {
    let mut buf = BytesMut::new();
    DataFlowHeader::server(FlowId::server(1), 0, FlowFlags::default()).encode(&mut buf);
    with_pingreq(buf.to_vec())
}

fn client_flow_with_server_id() -> Vec<u8> {
    let mut buf = BytesMut::new();
    DataFlowHeader::client(FlowId::server(1), 0, FlowFlags::default()).encode(&mut buf);
    with_pingreq(buf.to_vec())
}

fn second_control_flow() -> Vec<u8> {
    let mut buf = BytesMut::new();
    ControlFlowHeader::new(FlowFlags::default()).encode(&mut buf);
    with_pingreq(buf.to_vec())
}

async fn on_uni(bytes: &[u8]) -> (StreamEnd, Outcome) {
    let (broker, mut split) = connected_session("uni-flow").await;
    let mut stream = split.connection.open_uni().await.expect("open uni");
    stream.write_all(bytes).await.expect("write uni");
    let end = match tokio::time::timeout(Duration::from_secs(1), stream.stopped()).await {
        Ok(Ok(Some(code))) => StreamEnd::Stopped(code.into_inner()),
        Ok(_) => StreamEnd::Finished,
        Err(_) => StreamEnd::Open,
    };
    let result = outcome(&mut split).await;
    broker.stop().await;
    (end, result)
}

async fn on_bidi(bytes: &[u8], finish: bool) -> (StreamEnd, Outcome) {
    let (broker, mut split) = connected_session("bidi-flow").await;
    let (mut send, mut recv) = split.connection.open_bi().await.expect("open bidi");
    send.write_all(bytes).await.expect("write bidi");
    if finish {
        send.finish().expect("finish bidi");
    }
    let end = match tokio::time::timeout(Duration::from_secs(1), recv.read_to_end(64)).await {
        Ok(Ok(data)) if data.is_empty() => StreamEnd::Finished,
        Ok(Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(code)))) => {
            let stopped = tokio::time::timeout(Duration::from_secs(1), send.stopped()).await;
            assert!(
                matches!(stopped, Ok(Ok(Some(stop))) if stop == code),
                "send side was not stopped with the reset code: {stopped:?}"
            );
            StreamEnd::Refused(code.into_inner())
        }
        Ok(other) => panic!("unexpected bidi stream end: {other:?}"),
        Err(_) => StreamEnd::Open,
    };
    let result = outcome(&mut split).await;
    broker.stop().await;
    (end, result)
}

#[tokio::test]
async fn user_defined_flows_are_refused_without_closing_the_connection() {
    for flow in USER_DEFINED_FLOWS {
        assert_eq!(
            on_uni(flow).await,
            (
                StreamEnd::Stopped(code(QuicStreamCode::FlowRefused)),
                STILL_SERVED
            )
        );
        assert_eq!(
            on_bidi(flow, false).await,
            (
                StreamEnd::Refused(code(QuicStreamCode::FlowRefused)),
                STILL_SERVED
            )
        );
    }
}

#[tokio::test]
async fn flows_only_the_server_may_open_are_refused_as_a_bad_flow_id() {
    for flow in [server_flow(), client_flow_with_server_id()] {
        assert_eq!(
            on_uni(&flow).await,
            (
                StreamEnd::Stopped(code(QuicStreamCode::BadFlowId)),
                STILL_SERVED
            )
        );
        assert_eq!(
            on_bidi(&flow, false).await,
            (
                StreamEnd::Refused(code(QuicStreamCode::BadFlowId)),
                STILL_SERVED
            )
        );
    }
}

#[tokio::test]
async fn a_second_control_flow_is_refused_as_a_stream_type_error() {
    let flow = second_control_flow();
    assert_eq!(
        on_uni(&flow).await,
        (
            StreamEnd::Stopped(code(QuicStreamCode::StreamType)),
            STILL_SERVED
        )
    );
    assert_eq!(
        on_bidi(&flow, false).await,
        (
            StreamEnd::Refused(code(QuicStreamCode::StreamType)),
            STILL_SERVED
        )
    );
}

#[tokio::test]
async fn a_bidirectional_stream_without_a_flow_header_is_refused() {
    assert_eq!(
        on_bidi(&PINGREQ, false).await,
        (
            StreamEnd::Refused(code(QuicStreamCode::StreamType)),
            STILL_SERVED
        )
    );
}

#[tokio::test]
async fn a_unidirectional_client_data_flow_is_served() {
    assert_eq!(
        on_uni(&with_pingreq(data_flow(1, 0, FlowFlags::default()))).await,
        (StreamEnd::Open, STILL_SERVED)
    );
    assert_eq!(on_uni(&PINGREQ).await, (StreamEnd::Open, STILL_SERVED));
}

#[tokio::test]
async fn a_bidirectional_client_data_flow_is_refused() {
    assert_eq!(
        on_bidi(&with_pingreq(data_flow(1, 0, FlowFlags::default())), false).await,
        (
            StreamEnd::Refused(code(QuicStreamCode::FlowRefused)),
            STILL_SERVED
        )
    );
}

#[tokio::test]
async fn a_discard_header_ended_by_fin_is_answered_with_fin() {
    assert_eq!(
        on_bidi(&data_flow(1, 0, FlowFlags::discard()), true).await,
        (StreamEnd::Finished, STILL_SERVED)
    );
}

#[tokio::test]
async fn a_discard_header_followed_by_data_is_not_a_discard() {
    assert_eq!(
        on_bidi(&with_pingreq(data_flow(1, 0, FlowFlags::discard())), false).await,
        (
            StreamEnd::Refused(code(QuicStreamCode::FlowRefused)),
            STILL_SERVED
        )
    );
}

#[tokio::test]
async fn a_unidirectional_flow_ending_inside_a_packet_keeps_the_session() {
    let mut flow = data_flow(1, 0, FlowFlags::default());
    flow.extend_from_slice(&PINGREQ[..1]);
    let (broker, mut split) = connected_session("incomplete-flow").await;
    let mut stream = split.connection.open_uni().await.expect("open uni");
    stream.write_all(&flow).await.expect("write uni");
    stream.finish().expect("finish uni");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(outcome(&mut split).await, STILL_SERVED);
    broker.stop().await;
}

fn subscribe_frame(topic: &str) -> Vec<u8> {
    let mut body = vec![0x00, 0x01, 0x00];
    body.extend_from_slice(
        &u16::try_from(topic.len())
            .expect("short topic")
            .to_be_bytes(),
    );
    body.extend_from_slice(topic.as_bytes());
    body.push(0x00);
    let mut frame = vec![0x82, u8::try_from(body.len()).expect("short SUBSCRIBE")];
    frame.extend_from_slice(&body);
    frame
}

fn publish_frame(topic: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(
        &u16::try_from(topic.len())
            .expect("short topic")
            .to_be_bytes(),
    );
    body.extend_from_slice(topic.as_bytes());
    body.push(0x00);
    body.extend_from_slice(payload);
    let mut frame = vec![0x30, u8::try_from(body.len()).expect("short PUBLISH")];
    frame.extend_from_slice(&body);
    frame
}

async fn delivered_to_aborted_flow(discard: bool) -> bool {
    let (broker, mut split) = connected_session("discard-subscriptions").await;
    let preserved = FlowFlags {
        err_tolerance: 2,
        persistent_subscriptions: 1,
        ..FlowFlags::default()
    };
    let mut flow = split.connection.open_uni().await.expect("open flow");
    let mut bytes = data_flow(5, 300, preserved);
    bytes.extend_from_slice(&subscribe_frame("flows/discard"));
    flow.write_all(&bytes).await.expect("subscribe on flow");
    let mut suback = [0u8; 6];
    split
        .recv
        .read_exact(&mut suback)
        .await
        .expect("read SUBACK");
    assert_eq!(suback, [0x90, 0x04, 0x00, 0x01, 0x00, 0x00]);
    flow.reset(quinn::VarInt::from_u32(
        QuicStreamCode::FlowCancelled.code(),
    ))
    .expect("abort flow");
    tokio::time::sleep(Duration::from_millis(200)).await;

    if discard {
        let (mut send, mut recv) = split.connection.open_bi().await.expect("open discard");
        send.write_all(&data_flow(5, 0, FlowFlags::discard()))
            .await
            .expect("send discard");
        send.finish().expect("finish discard");
        let response = tokio::time::timeout(Duration::from_secs(2), recv.read_to_end(16))
            .await
            .expect("discard answered")
            .expect("discard ends with FIN");
        assert!(response.is_empty());
    }

    split
        .send
        .write_all(&publish_frame("flows/discard", b"after"))
        .await
        .expect("publish");
    let delivered = tokio::time::timeout(Duration::from_secs(2), split.connection.accept_bi())
        .await
        .is_ok();
    broker.stop().await;
    delivered
}

#[tokio::test]
async fn discarding_a_flow_removes_its_subscriptions() {
    assert!(delivered_to_aborted_flow(false).await);
    assert!(!delivered_to_aborted_flow(true).await);
}

#[derive(Clone, Copy)]
enum FlowEnd {
    Fin,
    Reset,
}

async fn subscribe_on_flow(
    split: &mut QuicSplitResult,
    expire: u64,
    flags: FlowFlags,
) -> quinn::SendStream {
    let mut flow = split.connection.open_uni().await.expect("open flow");
    let mut bytes = data_flow(5, expire, flags);
    bytes.extend_from_slice(&subscribe_frame("flows/expiry"));
    flow.write_all(&bytes).await.expect("subscribe on flow");
    let mut suback = [0u8; 6];
    split
        .recv
        .read_exact(&mut suback)
        .await
        .expect("read SUBACK");
    assert_eq!(suback, [0x90, 0x04, 0x00, 0x01, 0x00, 0x00]);
    flow
}

async fn delivered_on_flow_subscription(split: &mut QuicSplitResult) -> bool {
    split
        .send
        .write_all(&publish_frame("flows/expiry", b"after"))
        .await
        .expect("publish");
    tokio::time::timeout(Duration::from_secs(2), split.connection.accept_bi())
        .await
        .is_ok()
}

async fn delivered_after_flow_end(
    err_tolerance: u8,
    expire: u64,
    end: FlowEnd,
    wait: Duration,
) -> bool {
    let (broker, mut split) = connected_session("flow-expiry").await;
    let flags = FlowFlags {
        err_tolerance,
        persistent_subscriptions: 1,
        ..FlowFlags::default()
    };
    let mut flow = subscribe_on_flow(&mut split, expire, flags).await;
    match end {
        FlowEnd::Fin => flow.finish().expect("finish flow"),
        FlowEnd::Reset => flow
            .reset(quinn::VarInt::from_u32(
                QuicStreamCode::FlowCancelled.code(),
            ))
            .expect("abort flow"),
    }
    tokio::time::sleep(wait).await;
    let delivered = delivered_on_flow_subscription(&mut split).await;
    broker.stop().await;
    delivered
}

const SHORTLY: Duration = Duration::from_millis(200);

#[tokio::test]
async fn a_flow_ended_with_fin_drops_its_subscriptions() {
    assert!(!delivered_after_flow_end(0, 2, FlowEnd::Fin, SHORTLY).await);
    assert!(!delivered_after_flow_end(2, 2, FlowEnd::Fin, SHORTLY).await);
}

#[tokio::test]
async fn an_aborted_flow_keeps_its_subscriptions_until_it_expires() {
    for err_tolerance in [0, 2] {
        assert!(delivered_after_flow_end(err_tolerance, 1, FlowEnd::Reset, SHORTLY).await);
        assert!(
            !delivered_after_flow_end(
                err_tolerance,
                1,
                FlowEnd::Reset,
                Duration::from_millis(1500)
            )
            .await
        );
    }
}

#[tokio::test]
async fn an_aborted_flow_with_expire_zero_drops_its_subscriptions() {
    assert!(!delivered_after_flow_end(0, 0, FlowEnd::Reset, SHORTLY).await);
    assert!(!delivered_after_flow_end(2, 0, FlowEnd::Reset, SHORTLY).await);
}

fn connect_with_session_expiry(client_id: &str, clean_start: bool) -> Vec<u8> {
    let flags = if clean_start { 0x02 } else { 0x00 };
    let mut body = vec![
        0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05, flags, 0x00, 0x3C, 0x05, 0x11, 0x00, 0x00, 0x01,
        0x2C,
    ];
    body.extend_from_slice(
        &u16::try_from(client_id.len())
            .expect("client id fits u16")
            .to_be_bytes(),
    );
    body.extend_from_slice(client_id.as_bytes());
    let mut frame = vec![0x10, u8::try_from(body.len()).expect("short CONNECT")];
    frame.extend_from_slice(&body);
    frame
}

async fn persistent_session_at(addr: SocketAddr, clean_start: bool) -> (QuicSplitResult, u8) {
    let mut transport =
        QuicTransport::new(QuicConfig::new(addr, "localhost").with_verify_server_cert(false));
    transport.connect().await.expect("QUIC handshake");
    let mut split = transport.into_split().expect("split transport");
    split
        .send
        .write_all(&connect_with_session_expiry("flow-reconnect", clean_start))
        .await
        .expect("send CONNECT");
    let mut header = [0u8; 2];
    split
        .recv
        .read_exact(&mut header)
        .await
        .expect("read CONNACK header");
    let mut body = vec![0u8; usize::from(header[1])];
    split
        .recv
        .read_exact(&mut body)
        .await
        .expect("read CONNACK body");
    (split, body[0])
}

async fn delivered_after_reconnect(expire: u64, finish_first: bool) -> bool {
    let (broker, addr) = Broker::start().await;
    let (mut first, _) = persistent_session_at(addr, true).await;
    let flags = FlowFlags {
        persistent_subscriptions: 1,
        ..FlowFlags::default()
    };
    let mut flow = subscribe_on_flow(&mut first, expire, flags).await;
    if finish_first {
        flow.finish().expect("finish flow");
        tokio::time::sleep(SHORTLY).await;
    }
    first.connection.close(quinn::VarInt::from_u32(0), b"gone");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (mut second, session_present) = persistent_session_at(addr, false).await;
    assert_eq!(session_present, 1);
    let delivered = delivered_on_flow_subscription(&mut second).await;
    broker.stop().await;
    delivered
}

#[tokio::test]
async fn a_closed_connection_drops_subscriptions_of_flows_with_expire_zero() {
    assert!(delivered_after_reconnect(300, false).await);
    assert!(!delivered_after_reconnect(0, false).await);
    assert!(!delivered_after_reconnect(300, true).await);
}

const PACKET_LIMIT: usize = 1024;
const SIZE_TOPIC: &str = "size/check";

fn push_remaining_length(frame: &mut Vec<u8>, mut length: usize) {
    loop {
        let byte = u8::try_from(length % 128).expect("seven bits");
        length /= 128;
        if length == 0 {
            frame.push(byte);
            return;
        }
        frame.push(byte | 0x80);
    }
}

fn publish_of_size(total: usize) -> Vec<u8> {
    let remaining = total - 3;
    let mut frame = vec![0x30];
    push_remaining_length(&mut frame, remaining);
    assert_eq!(frame.len(), 3, "a two-byte remaining length");
    frame.extend_from_slice(
        &u16::try_from(SIZE_TOPIC.len())
            .expect("short topic")
            .to_be_bytes(),
    );
    frame.extend_from_slice(SIZE_TOPIC.as_bytes());
    frame.push(0x00);
    frame.resize(total, b'x');
    frame
}

async fn close_code(connection: &quinn::Connection) -> Option<u64> {
    tokio::time::timeout(Duration::from_secs(2), connection.closed())
        .await
        .ok()
        .and_then(|error| match error {
            quinn::ConnectionError::ApplicationClosed(close) => Some(close.error_code.into_inner()),
            _ => None,
        })
}

fn protocol_level_0() -> u64 {
    u64::from(QuicConnectionCode::ProtocolLevel0.code())
}

async fn subscribed_session(addr: SocketAddr) -> QuicSplitResult {
    let mut split = session_at(addr, "size-subscriber").await;
    split
        .send
        .write_all(&subscribe_frame(SIZE_TOPIC))
        .await
        .expect("send SUBSCRIBE");
    let mut suback = [0u8; 6];
    split
        .recv
        .read_exact(&mut suback)
        .await
        .expect("read SUBACK");
    assert_eq!(suback, [0x90, 0x04, 0x00, 0x01, 0x00, 0x00]);
    split
}

async fn first_byte(split: &mut QuicSplitResult) -> Option<u8> {
    let mut first = [0u8; 1];
    match tokio::time::timeout(Duration::from_secs(2), split.recv.read_exact(&mut first)).await {
        Ok(Ok(())) => Some(first[0]),
        _ => None,
    }
}

async fn ping(split: &mut QuicSplitResult) -> Option<u8> {
    split.send.write_all(&PINGREQ).await.ok()?;
    first_byte(split).await
}

#[derive(Clone, Copy, Debug)]
enum PublishPath {
    ControlStream,
    DataStream,
    Datagram,
}

const PUBLISH_PATHS: [PublishPath; 3] = [
    PublishPath::ControlStream,
    PublishPath::DataStream,
    PublishPath::Datagram,
];

async fn published_on(
    path: PublishPath,
    total: usize,
) -> (Broker, QuicSplitResult, QuicSplitResult) {
    let (broker, addr) =
        Broker::start_with(PACKET_LIMIT, ServerDeliveryStrategy::ControlOnly).await;
    let subscriber = subscribed_session(addr).await;
    let mut publisher = session_at(addr, "size-publisher").await;
    let frame = publish_of_size(total);
    match path {
        PublishPath::ControlStream => publisher
            .send
            .write_all(&frame)
            .await
            .expect("write control stream"),
        PublishPath::DataStream => {
            let mut stream = publisher.connection.open_uni().await.expect("open uni");
            let mut bytes = data_flow(1, 0, FlowFlags::default());
            bytes.extend_from_slice(&frame);
            stream.write_all(&bytes).await.expect("write data stream");
        }
        PublishPath::Datagram => publisher
            .connection
            .send_datagram(frame.into())
            .expect("send datagram"),
    }
    (broker, subscriber, publisher)
}

#[tokio::test]
async fn a_packet_of_the_maximum_size_is_delivered_on_every_path() {
    for path in PUBLISH_PATHS {
        let (broker, mut subscriber, mut publisher) = published_on(path, PACKET_LIMIT).await;
        assert_eq!(first_byte(&mut subscriber).await, Some(0x30), "{path:?}");
        assert_eq!(ping(&mut publisher).await, Some(0xD0), "{path:?}");
        broker.stop().await;
    }
}

#[tokio::test]
async fn a_packet_over_the_maximum_size_closes_the_connection_on_every_path() {
    for path in PUBLISH_PATHS {
        let (broker, mut subscriber, publisher) = published_on(path, PACKET_LIMIT + 1).await;
        assert_eq!(
            close_code(&publisher.connection).await,
            Some(protocol_level_0()),
            "{path:?}"
        );
        assert_eq!(ping(&mut subscriber).await, Some(0xD0), "{path:?}");
        broker.stop().await;
    }
}

#[tokio::test]
async fn a_data_stream_announcing_an_oversized_packet_closes_the_connection() {
    let (broker, addr) =
        Broker::start_with(PACKET_LIMIT, ServerDeliveryStrategy::ControlOnly).await;
    let publisher = session_at(addr, "size-announcer").await;
    let mut stream = publisher.connection.open_uni().await.expect("open uni");
    let mut header = data_flow(1, 0, FlowFlags::default());
    header.push(0x30);
    push_remaining_length(&mut header, 64 * 1024 * 1024);
    stream.write_all(&header).await.expect("write header");
    let chunk = vec![0u8; 64 * 1024];
    let mut written = 0usize;
    while written < 8 * 1024 * 1024 {
        match tokio::time::timeout(Duration::from_secs(2), stream.write_all(&chunk)).await {
            Ok(Ok(())) => written += chunk.len(),
            _ => break,
        }
    }
    let closed = close_code(&publisher.connection).await;
    broker.stop().await;
    assert_eq!(
        closed,
        Some(protocol_level_0()),
        "broker read {written} bytes of an oversized packet"
    );
}

#[tokio::test]
async fn an_oversized_ack_on_a_stream_the_broker_opened_closes_the_connection() {
    let (broker, addr) = Broker::start_with(PACKET_LIMIT, ServerDeliveryStrategy::PerTopic).await;
    let mut subscriber = session_at(addr, "ack-subscriber").await;
    let mut subscribe = subscribe_frame(SIZE_TOPIC);
    *subscribe.last_mut().expect("subscription options") = 0x01;
    subscriber
        .send
        .write_all(&subscribe)
        .await
        .expect("send SUBSCRIBE");
    let mut suback = [0u8; 6];
    subscriber
        .recv
        .read_exact(&mut suback)
        .await
        .expect("read SUBACK");
    assert_eq!(suback, [0x90, 0x04, 0x00, 0x01, 0x00, 0x01]);
    let mut publisher = session_at(addr, "ack-publisher").await;
    let mut publish = publish_frame(SIZE_TOPIC, b"hello");
    publish[0] = 0x32;
    publish[1] += 2;
    publish.splice(4 + SIZE_TOPIC.len()..4 + SIZE_TOPIC.len(), [0x00, 0x07]);
    publisher
        .send
        .write_all(&publish)
        .await
        .expect("send QoS 1 PUBLISH");
    let (mut send, mut recv) =
        tokio::time::timeout(Duration::from_secs(2), subscriber.connection.accept_bi())
            .await
            .expect("delivery stream opened")
            .expect("accept delivery stream");
    let mut head = [0u8; 1];
    recv.read_exact(&mut head)
        .await
        .expect("read delivery stream");
    let mut ack = vec![0x40];
    push_remaining_length(&mut ack, 2 * PACKET_LIMIT);
    ack.resize(ack.len() + 2 * PACKET_LIMIT, 0);
    send.write_all(&ack).await.expect("write oversized ack");
    let closed = close_code(&subscriber.connection).await;
    broker.stop().await;
    assert_eq!(closed, Some(protocol_level_0()));
}
