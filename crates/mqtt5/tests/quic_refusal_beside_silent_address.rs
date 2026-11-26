#![cfg(feature = "transport-quic")]

use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::{ConnectOptions, MqttClient, MqttError};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

const CONNACK_BAD_CREDENTIALS: [u8; 5] = [0x20, 0x03, 0x00, 0x86, 0x00];

fn server_config() -> quinn::ServerConfig {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert = rustls_pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key = rustls_pki_types::PrivateKeyDer::Pkcs8(rustls_pki_types::PrivatePkcs8KeyDer::from(
        certified.signing_key.serialize_der(),
    ));
    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    crypto.alpn_protocols = vec![b"mqtt".to_vec()];
    let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(crypto).unwrap();
    quinn::ServerConfig::with_crypto(Arc::new(quic_crypto))
}

fn serve_refusing(addr: SocketAddr, connects: Arc<AtomicUsize>) -> tokio::task::JoinHandle<()> {
    let endpoint = quinn::Endpoint::server(server_config(), addr).unwrap();
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let connects = connects.clone();
            tokio::spawn(async move {
                let Ok(connection) = incoming.await else {
                    return;
                };
                let Ok((mut send, mut recv)) = connection.accept_bi().await else {
                    return;
                };
                let mut buf = [0u8; 512];
                if !matches!(recv.read(&mut buf).await, Ok(Some(n)) if n > 0) {
                    return;
                }
                connects.fetch_add(1, Ordering::SeqCst);
                let _ = send.write_all(&CONNACK_BAD_CREDENTIALS).await;
                let _ = send.flush().await;
                connection.closed().await;
            });
        }
    })
}

#[tokio::test]
async fn refusal_on_one_address_beside_a_silent_one_stops_reconnection() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (v4, silent) = loop {
        let v4 = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = v4.local_addr().unwrap().port();
        if let Ok(v6) = std::net::UdpSocket::bind(("::1", port)) {
            drop(v4);
            break (SocketAddr::from(([127, 0, 0, 1], port)), v6);
        }
    };
    let resolved: Vec<SocketAddr> = tokio::net::lookup_host(("localhost", v4.port()))
        .await
        .unwrap()
        .collect();
    assert!(
        resolved.first().is_some_and(SocketAddr::is_ipv6),
        "precondition: localhost resolves ::1 first, got {resolved:?}"
    );
    let connects = Arc::new(AtomicUsize::new(0));
    let server = serve_refusing(v4, connects.clone());

    let client = MqttClient::with_options(
        ConnectOptions::new("refusal-beside-silent-refused").with_automatic_reconnect(true),
    );
    client
        .set_quic_connect_timeout(Duration::from_secs(1))
        .await;
    let started = Instant::now();
    let result = client
        .connect(&format!("quic://localhost:{}", v4.port()))
        .await;
    let elapsed = started.elapsed();
    tokio::time::sleep(Duration::from_secs(5)).await;
    let seen = connects.load(Ordering::SeqCst);
    let _ = client.disconnect().await;
    server.abort();
    drop(silent);
    tracing::info!(?result, ?elapsed, seen, "outcome");
    assert!(
        matches!(
            result,
            Err(MqttError::ConnectionRefused(
                ReasonCode::BadUsernameOrPassword
            ))
        ),
        "connect result {result:?} after {elapsed:?}, refusing server saw {seen} CONNECTs"
    );
    assert_eq!(
        seen, 1,
        "CONNECTs at the refusing server within 5 s after the refusal"
    );
}
