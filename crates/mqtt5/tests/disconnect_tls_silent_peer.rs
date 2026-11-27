#![cfg(feature = "broker")]

use mqtt5::{ConnectOptions, MqttClient, MqttError, WillMessage};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;

fn certs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_certs")
}

async fn silent_tls_server() -> (SocketAddr, Arc<AtomicUsize>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let chain = CertificateDer::pem_file_iter(certs().join("server.pem"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_file(certs().join("server.key")).unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handshakes = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&handshakes);
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            if let Ok(tls) = acceptor.accept(stream).await {
                counting.fetch_add(1, Ordering::SeqCst);
                held.push(tls);
            }
        }
    });
    (addr, handshakes)
}

async fn disconnect_against_silent_tls_peer(will_size: usize) -> (Duration, String) {
    let (addr, handshakes) = silent_tls_server().await;
    let client = MqttClient::with_options(
        ConnectOptions::new("cancel-tls-silent")
            .with_automatic_reconnect(false)
            .with_connect_packet_timeout(Duration::from_secs(5))
            .with_will(WillMessage::new("cancel/will", vec![7u8; will_size])),
    );
    client
        .set_tls_config(
            None,
            None,
            Some(std::fs::read(certs().join("ca.pem")).unwrap()),
        )
        .await;
    let connecting = client.clone();
    let url = format!("mqtts://localhost:{}", addr.port());
    let connect = tokio::spawn(async move { connecting.connect(&url).await });
    while handshakes.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    let disconnected = client.disconnect().await;
    let took = started.elapsed();
    let connected = connect.await.unwrap();
    assert!(
        matches!(disconnected, Err(MqttError::NotConnected)),
        "disconnect {disconnected:?}"
    );
    assert!(connected.is_err(), "connect {connected:?}");
    (
        took,
        format!("disconnect {disconnected:?}, connect {connected:?}"),
    )
}

#[tokio::test]
async fn disconnect_during_connack_to_a_tls_peer_that_never_reads_returns_promptly() {
    let (took, outcome) = disconnect_against_silent_tls_peer(16).await;
    assert!(outcome.contains("being opened"), "{outcome}");
    assert!(
        took < Duration::from_millis(500),
        "took {took:?}: {outcome}"
    );
}

#[tokio::test]
async fn disconnect_during_connack_with_a_60_kb_will_to_a_tls_peer_that_never_reads() {
    let (took, outcome) = disconnect_against_silent_tls_peer(60_000).await;
    assert!(outcome.contains("being opened"), "{outcome}");
    assert!(
        took < Duration::from_millis(500),
        "took {took:?}: {outcome}"
    );
}
