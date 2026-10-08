#![cfg(feature = "broker")]
#![cfg(feature = "transport-quic")]

use mqtt5::broker::config::{
    BrokerConfig, QuicConfig as BrokerQuicConfig, StorageBackend, StorageConfig,
};
use mqtt5::broker::MqttBroker;
use mqtt5::transport::{QuicConfig, QuicSplitResult, QuicTransport};
use mqtt5::Transport;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

const CERT: &str = "../../test_certs/server.pem";
const KEY: &str = "../../test_certs/server.key";
const PINGREQ: [u8; 2] = [0xC0, 0x00];
const PINGREQ_WITH_FLAGS: [u8; 2] = [0xC1, 0x00];
const PUBLISH_QOS_3: [u8; 6] = [0x36, 0x04, 0x00, 0x01, b't', 0x00];
const DISCONNECT: [u8; 2] = [0xE0, 0x00];
const NO_ERROR: u64 = 0x00;
const PROTOCOL_ERROR_LEVEL_0: u64 = 0xB4;

#[derive(Debug, PartialEq)]
struct Outcome {
    session_alive: bool,
    close_code: Option<u64>,
}

const STILL_SERVED: Outcome = Outcome {
    session_alive: true,
    close_code: None,
};

const CLOSED_AS_PROTOCOL_ERROR: Outcome = Outcome {
    session_alive: false,
    close_code: Some(PROTOCOL_ERROR_LEVEL_0),
};

async fn start_broker() -> SocketAddr {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = BrokerConfig::default()
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_quic(
            BrokerQuicConfig::new(PathBuf::from(CERT), PathBuf::from(KEY))
                .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address")),
        );
    let mut broker = MqttBroker::with_config(config)
        .await
        .expect("broker starts");
    let addr = broker.quic_local_addr().expect("QUIC endpoint bound");
    tokio::spawn(async move { broker.run().await });
    addr
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

async fn connected_session(client_id: &str) -> QuicSplitResult {
    let addr = start_broker().await;
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

async fn on_control_stream(bytes: &[u8]) -> Outcome {
    let mut split = connected_session("malformed-control").await;
    split.send.write_all(bytes).await.expect("send on control");
    tokio::time::sleep(Duration::from_millis(200)).await;
    outcome(&mut split).await
}

async fn on_data_stream(bytes: &[u8]) -> Outcome {
    let mut split = connected_session("malformed-data").await;
    let mut data = split.connection.open_uni().await.expect("open data stream");
    data.write_all(bytes).await.expect("send on data stream");
    tokio::time::sleep(Duration::from_millis(200)).await;
    outcome(&mut split).await
}

async fn in_datagram(bytes: &[u8]) -> Outcome {
    let mut split = connected_session("malformed-datagram").await;
    split
        .connection
        .send_datagram(bytes.to_vec().into())
        .expect("send datagram");
    tokio::time::sleep(Duration::from_millis(200)).await;
    outcome(&mut split).await
}

#[tokio::test]
async fn valid_packet_on_the_control_stream_keeps_the_session() {
    assert_eq!(on_control_stream(&PINGREQ).await, STILL_SERVED);
}

#[tokio::test]
async fn malformed_packet_on_the_control_stream_closes_the_connection() {
    assert_eq!(
        on_control_stream(&PINGREQ_WITH_FLAGS).await,
        CLOSED_AS_PROTOCOL_ERROR
    );
    assert_eq!(
        on_control_stream(&PUBLISH_QOS_3).await,
        CLOSED_AS_PROTOCOL_ERROR
    );
}

#[tokio::test]
async fn valid_packet_on_a_data_stream_keeps_the_session() {
    assert_eq!(on_data_stream(&PINGREQ).await, STILL_SERVED);
}

#[tokio::test]
async fn malformed_packet_on_a_data_stream_closes_the_connection() {
    assert_eq!(
        on_data_stream(&PINGREQ_WITH_FLAGS).await,
        CLOSED_AS_PROTOCOL_ERROR
    );
    assert_eq!(
        on_data_stream(&PUBLISH_QOS_3).await,
        CLOSED_AS_PROTOCOL_ERROR
    );
}

#[tokio::test]
async fn valid_datagram_keeps_the_session() {
    assert_eq!(in_datagram(&PINGREQ).await, STILL_SERVED);
}

#[tokio::test]
async fn malformed_datagram_closes_the_connection() {
    assert_eq!(
        in_datagram(&PINGREQ_WITH_FLAGS).await,
        CLOSED_AS_PROTOCOL_ERROR
    );
    assert_eq!(in_datagram(&PUBLISH_QOS_3).await, CLOSED_AS_PROTOCOL_ERROR);
}

#[tokio::test]
async fn client_resetting_a_data_stream_keeps_the_session() {
    let mut split = connected_session("reset-data").await;
    let mut data = split.connection.open_uni().await.expect("open data stream");
    data.write_all(&PINGREQ[..1])
        .await
        .expect("send partial packet");
    data.reset(quinn::VarInt::from_u32(0xBC))
        .expect("reset data stream");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(outcome(&mut split).await, STILL_SERVED);
}

#[tokio::test]
async fn client_finishing_a_data_stream_keeps_the_session() {
    let mut split = connected_session("finish-data").await;
    let mut data = split.connection.open_uni().await.expect("open data stream");
    data.write_all(&PINGREQ).await.expect("send on data stream");
    data.finish().expect("finish data stream");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(outcome(&mut split).await, STILL_SERVED);
}

#[tokio::test]
async fn connection_is_closed_without_error_after_a_client_disconnect() {
    let mut split = connected_session("disconnect").await;
    split
        .send
        .write_all(&DISCONNECT)
        .await
        .expect("send DISCONNECT");
    let close_code = tokio::time::timeout(Duration::from_secs(3), split.connection.closed())
        .await
        .ok()
        .and_then(|error| match error {
            quinn::ConnectionError::ApplicationClosed(close) => Some(close.error_code.into_inner()),
            _ => None,
        });
    assert_eq!(close_code, Some(NO_ERROR));
}
