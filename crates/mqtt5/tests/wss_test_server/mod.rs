//! A loopback WSS server for the wss:// TLS tests, reporting what it observed
//! about each TLS handshake.
//!
//! Requires the certificates from `scripts/generate_test_certs.sh`.
#![allow(dead_code)]

use mqtt5::transport::websocket::{WebSocketConfig, WebSocketTransport};
use mqtt5::Transport;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub const WAIT: Duration = Duration::from_secs(5);
pub const CA: &str = "../../test_certs/ca.pem";
pub const SERVER_CERT: &str = "../../test_certs/server.pem";
pub const SERVER_KEY: &str = "../../test_certs/server.key";
pub const CLIENT_CERT: &str = "../../test_certs/client.pem";
pub const CLIENT_KEY: &str = "../../test_certs/client.key";

/// What the server observed about a TLS handshake that completed.
#[derive(Debug)]
pub struct Handshake {
    pub alpn: Option<Vec<u8>>,
    pub client_presented_cert: bool,
}

pub fn certs(path: &str) -> Vec<CertificateDer<'static>> {
    CertificateDer::pem_file_iter(path)
        .unwrap_or_else(|e| panic!("read {path}: {e}"))
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

pub fn key(path: &str) -> PrivateKeyDer<'static> {
    PrivateKeyDer::from_pem_file(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

pub fn server_config(
    cert: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    require_client_cert: bool,
    alpn: &[&[u8]],
) -> ServerConfig {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let builder = ServerConfig::builder();
    let mut config = if require_client_cert {
        let mut roots = RootCertStore::empty();
        for ca in certs(CA) {
            roots.add(ca).expect("add CA");
        }
        let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .expect("client verifier");
        builder.with_client_cert_verifier(verifier)
    } else {
        builder.with_no_client_auth()
    }
    .with_single_cert(cert, key)
    .expect("server cert");
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    config
}

/// Starts a one-connection WSS server. The receiver yields the handshake
/// details if the TLS and WebSocket handshakes both complete.
pub async fn wss_server(config: ServerConfig) -> (SocketAddr, oneshot::Receiver<Handshake>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(tls) = acceptor.accept(stream).await else {
            return;
        };
        let (_, session) = tls.get_ref();
        let handshake = Handshake {
            alpn: session.alpn_protocol().map(<[u8]>::to_vec),
            client_presented_cert: session.peer_certificates().is_some(),
        };
        let Ok(ws) = tokio_tungstenite::accept_hdr_async(tls, SelectMqtt).await else {
            return;
        };
        let _ = tx.send(handshake);
        // Hold the connection open until the client closes it.
        let _ws = ws;
        tokio::time::sleep(WAIT).await;
    });
    (addr, rx)
}

/// Selects the "mqtt" subprotocol the client offers by default, as a broker
/// would; the client fails the handshake if the server selects none.
pub struct SelectMqtt;

impl Callback for SelectMqtt {
    fn on_request(self, _req: &Request, mut resp: Response) -> Result<Response, ErrorResponse> {
        resp.headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("mqtt"));
        Ok(resp)
    }
}

pub fn wss_config(addr: SocketAddr) -> WebSocketConfig {
    WebSocketConfig::new(&format!("wss://{addr}/mqtt")).expect("config")
}

/// Connects and returns the server's view of the handshake, or the client's
/// error if the connection failed.
pub async fn connect(
    config: WebSocketConfig,
    server: oneshot::Receiver<Handshake>,
) -> Result<Handshake, String> {
    let mut transport = WebSocketTransport::new(config);
    let result = timeout(WAIT, transport.connect())
        .await
        .expect("connect timed out");
    match result {
        Ok(()) => {
            let handshake = timeout(WAIT, server)
                .await
                .expect("server timed out")
                .expect("server saw no completed handshake");
            transport.close().await.expect("close");
            Ok(handshake)
        }
        Err(e) => Err(e.to_string()),
    }
}
