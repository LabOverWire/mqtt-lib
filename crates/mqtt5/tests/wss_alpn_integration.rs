mod common;
use common::{find_workspace_root, TestBroker};
use mqtt5::broker::config::{
    BrokerConfig, StorageBackend, StorageConfig, TlsConfig as BrokerTlsConfig, WebSocketConfig,
};
use rustls::pki_types::ServerName;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use tokio::net::TcpStream;

static WSS_PORT_COUNTER: AtomicU16 = AtomicU16::new(21100);

async fn start_wss_broker() -> (TestBroker, u16) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let workspace_root = find_workspace_root();
    let wss_port = WSS_PORT_COUNTER.fetch_add(1, Ordering::SeqCst);

    let tls_config = BrokerTlsConfig::new(
        workspace_root.join("test_certs/server.pem"),
        workspace_root.join("test_certs/server.key"),
    )
    .with_require_client_cert(false)
    .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap());

    let ws_tls_config = WebSocketConfig::new()
        .with_bind_addresses(vec![format!("127.0.0.1:{wss_port}").parse().unwrap()])
        .with_path("/mqtt".to_string());

    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };

    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config)
        .with_tls(tls_config)
        .with_websocket_tls(ws_tls_config);

    let broker = TestBroker::start_with_config(config).await;
    (broker, wss_port)
}

fn build_tls_connector(alpn_protocols: Vec<Vec<u8>>) -> tokio_rustls::TlsConnector {
    let mut root_store = rustls::RootCertStore::empty();
    let workspace_root = find_workspace_root();
    let ca_pem = std::fs::read(workspace_root.join("test_certs/ca.pem")).unwrap();
    let ca_certs: Vec<_> = rustls_pemfile::certs(&mut &ca_pem[..])
        .filter_map(std::result::Result::ok)
        .collect();
    for cert in ca_certs {
        root_store.add(cert).unwrap();
    }

    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    config.alpn_protocols = alpn_protocols;

    tokio_rustls::TlsConnector::from(Arc::new(config))
}

#[tokio::test]
async fn test_wss_accepts_http11_alpn() {
    let (_broker, wss_port) = start_wss_broker().await;

    let connector = build_tls_connector(vec![b"http/1.1".to_vec()]);
    let tcp = TcpStream::connect(format!("127.0.0.1:{wss_port}"))
        .await
        .unwrap();
    let server_name = ServerName::try_from("localhost").unwrap();

    let result = connector.connect(server_name, tcp).await;
    assert!(
        result.is_ok(),
        "WSS port should accept http/1.1 ALPN: {:?}",
        result.err()
    );

    let tls_stream = result.unwrap();
    let negotiated = tls_stream.get_ref().1.alpn_protocol();
    assert_eq!(
        negotiated,
        Some(b"http/1.1".as_slice()),
        "negotiated ALPN should be http/1.1"
    );
}

#[tokio::test]
async fn test_wss_rejects_mqtt_only_alpn() {
    let (_broker, wss_port) = start_wss_broker().await;

    let connector = build_tls_connector(vec![b"mqtt".to_vec()]);
    let tcp = TcpStream::connect(format!("127.0.0.1:{wss_port}"))
        .await
        .unwrap();
    let server_name = ServerName::try_from("localhost").unwrap();

    let result = connector.connect(server_name, tcp).await;
    assert!(
        result.is_err(),
        "WSS port should reject mqtt-only ALPN (no overlap with http/1.1)"
    );
}

#[tokio::test]
async fn test_wss_accepts_browser_alpn() {
    let (_broker, wss_port) = start_wss_broker().await;

    let connector = build_tls_connector(vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
    let tcp = TcpStream::connect(format!("127.0.0.1:{wss_port}"))
        .await
        .unwrap();
    let server_name = ServerName::try_from("localhost").unwrap();

    let result = connector.connect(server_name, tcp).await;
    assert!(
        result.is_ok(),
        "WSS port should accept browser ALPN (h2 + http/1.1): {:?}",
        result.err()
    );

    let tls_stream = result.unwrap();
    let negotiated = tls_stream.get_ref().1.alpn_protocol();
    assert_eq!(
        negotiated,
        Some(b"http/1.1".as_slice()),
        "should negotiate http/1.1 (server only offers http/1.1)"
    );
}
