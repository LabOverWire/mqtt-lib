use mqtt5::{ConnectOptions, MqttClient};
use rustls::crypto::{ring, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::test]
async fn tls_paths_use_the_installed_crypto_provider() {
    let restricted = CryptoProvider {
        cipher_suites: vec![ring::cipher_suite::TLS13_CHACHA20_POLY1305_SHA256],
        ..ring::default_provider()
    };
    restricted.install_default().unwrap();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let der = CertificateDer::from(cert.cert.der().to_vec());
    let key = PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap();
    let server_config =
        rustls::ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![der], key)
            .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tls = acceptor.accept(tcp).await.unwrap();
        tls.get_ref()
            .1
            .negotiated_cipher_suite()
            .map(|suite| suite.suite())
    });

    let client = MqttClient::with_options(
        ConnectOptions::new("installed-provider").with_automatic_reconnect(false),
    );
    client
        .set_tls_config(None, None, Some(cert.cert.pem().into_bytes()))
        .await
        .unwrap();
    let connecting = tokio::spawn(async move {
        tokio::time::timeout(
            Duration::from_secs(2),
            client.connect(&format!("mqtts://localhost:{port}")),
        )
        .await
    });

    let negotiated = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    connecting.abort();
    assert_eq!(
        negotiated,
        Some(rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256),
        "the mqtts client offered cipher suites outside the installed CryptoProvider"
    );
}
