#![cfg(feature = "transport-websocket")]

//! Checks that a `wss://` connection uses `WebSocketConfig::tls_config`: its
//! root certificates, client certificate and ALPN protocols, as seen by a
//! TLS server, rather than only being stored on the configuration.
//!
//! Requires the certificates from `scripts/generate_test_certs.sh`.

use mqtt5::transport::tls::TlsConfig;
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

const WAIT: Duration = Duration::from_secs(5);
const CA: &str = "../../test_certs/ca.pem";
const SERVER_CERT: &str = "../../test_certs/server.pem";
const SERVER_KEY: &str = "../../test_certs/server.key";
const CLIENT_CERT: &str = "../../test_certs/client.pem";
const CLIENT_KEY: &str = "../../test_certs/client.key";

/// What the server observed about a TLS handshake that completed.
#[derive(Debug)]
struct Handshake {
    alpn: Option<Vec<u8>>,
    client_presented_cert: bool,
}

fn certs(path: &str) -> Vec<CertificateDer<'static>> {
    CertificateDer::pem_file_iter(path)
        .unwrap_or_else(|e| panic!("read {path}: {e}"))
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

fn key(path: &str) -> PrivateKeyDer<'static> {
    PrivateKeyDer::from_pem_file(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn server_config(
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
async fn wss_server(config: ServerConfig) -> (SocketAddr, oneshot::Receiver<Handshake>) {
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
struct SelectMqtt;

impl Callback for SelectMqtt {
    fn on_request(self, _req: &Request, mut resp: Response) -> Result<Response, ErrorResponse> {
        resp.headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("mqtt"));
        Ok(resp)
    }
}

fn wss_config(addr: SocketAddr) -> WebSocketConfig {
    WebSocketConfig::new(&format!("wss://{addr}/mqtt")).expect("config")
}

/// Connects and returns the server's view of the handshake, or the client's
/// error if the connection failed.
async fn connect(
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

#[tokio::test]
async fn custom_ca_from_tls_config_is_trusted() {
    let (addr, server) = wss_server(server_config(
        certs(SERVER_CERT),
        key(SERVER_KEY),
        false,
        &[],
    ))
    .await;
    let config = wss_config(addr)
        .with_ca_cert_from_file(CA)
        .expect("load CA");

    connect(config, server)
        .await
        .expect("server certificate issued by the configured CA was rejected");
}

#[tokio::test]
async fn certificate_from_another_ca_is_rejected() {
    let other = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
        .expect("generate certificate");
    let (addr, server) = wss_server(server_config(
        vec![other.cert.der().clone()],
        PrivateKeyDer::try_from(other.signing_key.serialize_der()).expect("key"),
        false,
        &[],
    ))
    .await;
    let config = wss_config(addr)
        .with_ca_cert_from_file(CA)
        .expect("load CA");

    let err = connect(config, server)
        .await
        .expect_err("certificate not issued by the configured CA was accepted");
    assert!(
        err.contains("certificate") || err.contains("Certificate"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn client_certificate_from_tls_config_is_presented() {
    let (addr, server) = wss_server(server_config(
        certs(SERVER_CERT),
        key(SERVER_KEY),
        true,
        &[],
    ))
    .await;
    let config = wss_config(addr)
        .with_client_auth_from_files(CLIENT_CERT, CLIENT_KEY)
        .expect("load client auth")
        .with_ca_cert_from_file(CA)
        .expect("load CA");

    let handshake = connect(config, server)
        .await
        .expect("server requiring a client certificate rejected the connection");
    assert!(handshake.client_presented_cert);
}

#[tokio::test]
async fn client_certificate_is_presented_with_verification_disabled() {
    let (addr, server) = wss_server(server_config(
        certs(SERVER_CERT),
        key(SERVER_KEY),
        true,
        &[],
    ))
    .await;
    let mut tls_config = TlsConfig::new(addr, "127.0.0.1").with_verify_server_cert(false);
    tls_config
        .load_client_cert_pem(CLIENT_CERT)
        .expect("load client cert");
    tls_config
        .load_client_key_pem(CLIENT_KEY)
        .expect("load client key");
    let config = wss_config(addr).with_tls_config(tls_config);

    let handshake = connect(config, server)
        .await
        .expect("server requiring a client certificate rejected the connection");
    assert!(handshake.client_presented_cert);
}

#[tokio::test]
async fn alpn_protocols_from_tls_config_are_offered() {
    let (addr, server) = wss_server(server_config(
        certs(SERVER_CERT),
        key(SERVER_KEY),
        false,
        &[b"mqtt-test", b"http/1.1"],
    ))
    .await;
    let mut tls_config = TlsConfig::new(addr, "127.0.0.1").with_alpn_protocols(&["mqtt-test"]);
    tls_config.load_ca_cert_pem(CA).expect("load CA");
    let config = wss_config(addr).with_tls_config(tls_config);

    let handshake = connect(config, server).await.expect("connect");
    assert_eq!(handshake.alpn.as_deref(), Some(&b"mqtt-test"[..]));
}

#[tokio::test]
async fn without_tls_config_private_ca_is_not_trusted() {
    let (addr, server) = wss_server(server_config(
        certs(SERVER_CERT),
        key(SERVER_KEY),
        false,
        &[],
    ))
    .await;

    let err = connect(wss_config(addr), server)
        .await
        .expect_err("certificate from a private CA was accepted with no TLS configuration");
    assert!(
        err.contains("certificate") || err.contains("Certificate"),
        "unexpected error: {err}"
    );
}
