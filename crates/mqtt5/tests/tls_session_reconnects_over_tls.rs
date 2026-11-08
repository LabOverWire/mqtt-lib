use mqtt5::transport::tls::TlsConfig;
use mqtt5::{ConnectOptions, MqttClient};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CONNACK_OK: [u8; 5] = [0x20, 3, 0x00, 0x00, 0x00];
const TLS_RECORD_HANDSHAKE: u8 = 0x16;

fn tls_acceptor() -> tokio_rustls::TlsAcceptor {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let chain = vec![rustls::pki_types::CertificateDer::from(
        cert.cert.der().to_vec(),
    )];
    let key = rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
}

#[tokio::test]
async fn a_monitor_from_a_failed_connect_reconnects_a_tls_session_over_tls() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let refused = format!("mqtt://{}", closed.local_addr().unwrap());
    drop(closed);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = tls_acceptor();
    let first_bytes = Arc::new(Mutex::new(Vec::new()));
    let seen = first_bytes.clone();
    tokio::spawn(async move {
        let mut connections = 0;
        while let Ok((stream, _)) = listener.accept().await {
            connections += 1;
            let mut peek = [0u8; 1];
            let _ = stream.peek(&mut peek).await;
            seen.lock().unwrap().push(peek[0]);
            if connections == 1 {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let mut buf = [0u8; 4096];
                    let _ = tls.read(&mut buf).await;
                    tls.write_all(&CONNACK_OK).await.unwrap();
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    });

    let options = ConnectOptions::new("tls-stays-tls")
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_secs(2), Duration::from_secs(2))
        .with_credentials("user", "secret");
    let client = MqttClient::with_options(options.clone());
    assert!(client
        .connect_with_options(&refused, options.clone())
        .await
        .is_err());
    let tls = TlsConfig::new(addr, "localhost").with_verify_server_cert(false);
    client
        .connect_with_tls_and_options(tls, options)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(8)).await;
    let _ = client.disconnect().await;

    let bytes = first_bytes.lock().unwrap().clone();
    assert!(bytes.len() >= 2, "no reconnection happened: {bytes:02x?}");
    assert!(
        bytes.iter().all(|byte| *byte == TLS_RECORD_HANDSHAKE),
        "a connection did not start with TLS: {bytes:02x?}"
    );
}
