#![cfg(feature = "broker")]
#![cfg(feature = "transport-quic")]

use mqtt5::broker::config::{
    BrokerConfig, QuicConfig as BrokerQuicConfig, StorageBackend, StorageConfig,
};
use mqtt5::broker::quic_acceptor::QuicAcceptorConfig;
use mqtt5::broker::MqttBroker;
use mqtt5::transport::{QuicConfig, QuicSplitResult, QuicTransport};
use mqtt5::Transport;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

const CERT: &str = "../../test_certs/server.pem";
const KEY: &str = "../../test_certs/server.key";

fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

async fn start_broker(quic_config: BrokerQuicConfig) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    install_crypto_provider();
    let config = BrokerConfig::default()
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_quic(quic_config);
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let quic_addr = broker
        .quic_local_addr()
        .expect("QUIC endpoint must be bound");
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    (quic_addr, handle)
}

fn broker_quic_config() -> BrokerQuicConfig {
    BrokerQuicConfig::new(PathBuf::from(CERT), PathBuf::from(KEY))
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
}

async fn connect(config: QuicConfig) -> QuicSplitResult {
    let mut transport = QuicTransport::new(config);
    transport.connect().await.unwrap();
    transport.into_split().unwrap()
}

fn client_config(addr: SocketAddr) -> QuicConfig {
    QuicConfig::new(addr, "localhost").with_verify_server_cert(false)
}

async fn open_uni_streams(connection: &quinn::Connection, count: usize) -> Vec<quinn::SendStream> {
    let mut streams = Vec::with_capacity(count);
    for _ in 0..count {
        match tokio::time::timeout(Duration::from_millis(200), connection.open_uni()).await {
            Ok(Ok(stream)) => streams.push(stream),
            Ok(Err(e)) => panic!("opening a uni stream failed: {e}"),
            Err(_) => break,
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    streams
}

async fn burst_on_one_stream(connection: &quinn::Connection, bytes: usize) {
    let mut stream = connection.open_uni().await.unwrap();
    let payload = vec![0u8; bytes];
    let _ = tokio::time::timeout(Duration::from_millis(500), stream.write_all(&payload)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn broker_stream_limit_stops_a_client_opening_more_streams() {
    let (addr, broker) = start_broker(broker_quic_config().with_max_concurrent_streams(2)).await;
    let split = connect(client_config(addr)).await;

    let opened = open_uni_streams(&split.connection, 20).await;

    assert_eq!(opened.len(), 2);
    broker.abort();
}

#[tokio::test]
async fn default_broker_stream_limit_does_not_block_twenty_streams() {
    let (addr, broker) = start_broker(broker_quic_config()).await;
    let split = connect(client_config(addr)).await;

    let opened = open_uni_streams(&split.connection, 20).await;

    assert_eq!(opened.len(), 20);
    broker.abort();
}

#[tokio::test]
async fn broker_stream_window_blocks_a_burst_on_one_stream() {
    let (addr, broker) = start_broker(broker_quic_config().with_stream_receive_window(2048)).await;
    let split = connect(client_config(addr)).await;

    burst_on_one_stream(&split.connection, 65_536).await;

    assert!(split.connection.stats().frame_tx.stream_data_blocked > 0);
    broker.abort();
}

#[tokio::test]
async fn default_broker_stream_window_does_not_block_the_same_burst() {
    let (addr, broker) = start_broker(broker_quic_config()).await;
    let split = connect(client_config(addr)).await;

    burst_on_one_stream(&split.connection, 65_536).await;

    assert_eq!(split.connection.stats().frame_tx.stream_data_blocked, 0);
    broker.abort();
}

async fn streams_opened_toward_client(client_limit: Option<usize>) -> usize {
    install_crypto_provider();
    let certs = QuicAcceptorConfig::load_cert_chain_from_file(CERT)
        .await
        .unwrap();
    let key = QuicAcceptorConfig::load_private_key_from_file(KEY)
        .await
        .unwrap();
    let server_config = QuicAcceptorConfig::new(certs, key)
        .build_server_config()
        .unwrap();
    let endpoint = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = endpoint.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let incoming = endpoint.accept().await.unwrap();
        let connection = incoming.await.unwrap();
        let opened = open_uni_streams(&connection, 20).await.len();
        (opened, endpoint)
    });

    let config = match client_limit {
        Some(max) => client_config(addr).with_max_concurrent_streams(max),
        None => client_config(addr),
    };
    let split = connect(config).await;

    let (opened, endpoint) = server.await.unwrap();
    drop(split);
    endpoint.close(0u32.into(), b"done");
    opened
}

#[tokio::test]
async fn client_stream_limit_stops_a_server_opening_more_streams() {
    assert_eq!(streams_opened_toward_client(Some(2)).await, 2);
}

#[tokio::test]
async fn default_client_stream_limit_does_not_block_twenty_streams() {
    assert_eq!(streams_opened_toward_client(None).await, 20);
}
