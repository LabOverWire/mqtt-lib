#![cfg(feature = "transport-websocket")]

//! Checks that `use_system_roots` on a wss:// connection's TLS configuration
//! means the platform's root certificates, as it does for a wss:// connection
//! with no TLS configuration.
//!
//! `SSL_CERT_FILE` stands in for a CA installed on the platform:
//! rustls-native-certs loads that file instead of the platform store. The
//! variable is process-wide, so these tests have their own binary.
//!
//! Requires the certificates from `scripts/generate_test_certs.sh`.

mod wss_test_server;

use mqtt5::transport::tls::TlsConfig;
use mqtt5::{ConnectOptions, MqttClient};
use std::sync::Once;
use tokio::time::timeout;
use wss_test_server::{
    certs, connect, key, server_config, wss_config, wss_server, CA, CLIENT_CERT, CLIENT_KEY,
    SERVER_CERT, SERVER_KEY, WAIT,
};

/// Makes the test CA a platform root for this process. Every test calls it
/// before connecting, and it writes the variable only once.
fn install_test_ca_as_platform_root() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| std::env::set_var("SSL_CERT_FILE", CA));
}

#[tokio::test]
async fn platform_roots_are_used_without_a_configured_ca() {
    install_test_ca_as_platform_root();
    let (addr, server) = wss_server(server_config(
        certs(SERVER_CERT),
        key(SERVER_KEY),
        false,
        &[b"mqtt-test"],
    ))
    .await;
    let tls_config = TlsConfig::new(addr, "127.0.0.1").with_alpn_protocols(&["mqtt-test"]);

    let handshake = connect(wss_config(addr).with_tls_config(tls_config), server)
        .await
        .expect("server certificate from a platform-installed CA was rejected");
    assert_eq!(handshake.alpn.as_deref(), Some(&b"mqtt-test"[..]));
}

#[tokio::test]
async fn platform_roots_are_not_used_when_system_roots_are_off() {
    install_test_ca_as_platform_root();
    let (addr, server) = wss_server(server_config(
        certs(SERVER_CERT),
        key(SERVER_KEY),
        false,
        &[],
    ))
    .await;
    let tls_config = TlsConfig::new(addr, "127.0.0.1").with_system_roots(false);

    let err = connect(wss_config(addr).with_tls_config(tls_config), server)
        .await
        .expect_err("server certificate was trusted with no roots configured");
    assert!(err.contains("UnknownIssuer"), "unexpected error: {err}");
}

#[tokio::test]
async fn mqtt_client_with_cert_and_key_but_no_ca_trusts_platform_roots() {
    install_test_ca_as_platform_root();
    let (addr, server) = wss_server(server_config(
        certs(SERVER_CERT),
        key(SERVER_KEY),
        true,
        &[],
    ))
    .await;
    let client = MqttClient::with_options(
        ConnectOptions::new("wss-platform-roots").with_automatic_reconnect(false),
    );
    client
        .set_tls_config(
            Some(std::fs::read(CLIENT_CERT).expect("read client cert")),
            Some(std::fs::read(CLIENT_KEY).expect("read client key")),
            None,
        )
        .await;

    // The test server stops after the WebSocket handshake and never answers
    // CONNECT, so only the TLS handshake it observed is checked.
    let connecting = tokio::spawn(async move {
        let _ = client.connect(&format!("wss://{addr}/mqtt")).await;
    });
    let handshake = timeout(WAIT, server)
        .await
        .expect("server timed out")
        .expect("server saw no completed handshake: the platform roots were not used");
    assert!(handshake.client_presented_cert);
    connecting.abort();
}
