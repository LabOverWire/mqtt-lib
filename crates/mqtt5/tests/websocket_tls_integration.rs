#![cfg(feature = "transport-websocket")]

//! Checks that a `wss://` connection uses `WebSocketConfig::tls_config`: its
//! root certificates, client certificate and ALPN protocols, as seen by a
//! TLS server, rather than only being stored on the configuration.
//!
//! Requires the certificates from `scripts/generate_test_certs.sh`.

mod wss_test_server;

use mqtt5::transport::tls::TlsConfig;
use mqtt5::transport::websocket::WebSocketConfig;
use mqtt5::{ConnectOptions, MqttClient};
use rustls::pki_types::PrivateKeyDer;
use tokio::time::timeout;
use wss_test_server::{
    certs, connect, key, server_config, wss_config, wss_server, CA, CLIENT_CERT, CLIENT_KEY,
    SERVER_CERT, SERVER_KEY, WAIT,
};

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
    assert!(err.contains("UnknownIssuer"), "unexpected error: {err}");
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
    assert!(err.contains("UnknownIssuer"), "unexpected error: {err}");
}

#[tokio::test]
async fn custom_ca_works_with_a_host_name_url() {
    let (addr, server) = wss_server(server_config(
        certs(SERVER_CERT),
        key(SERVER_KEY),
        false,
        &[],
    ))
    .await;
    let config = WebSocketConfig::new(&format!("wss://localhost:{}/mqtt", addr.port()))
        .expect("config")
        .with_ca_cert_from_file(CA)
        .expect("a host name URL should accept a CA");

    connect(config, server)
        .await
        .expect("server certificate for localhost, issued by the configured CA, was rejected");
}

#[tokio::test]
async fn certificate_is_verified_against_the_url_host_not_tls_config_hostname() {
    let (addr, server) = wss_server(server_config(
        certs(SERVER_CERT),
        key(SERVER_KEY),
        false,
        &[],
    ))
    .await;
    let mut tls_config = TlsConfig::new(addr, "wrong.example");
    tls_config.load_ca_cert_pem(CA).expect("load CA");
    let config = wss_config(addr).with_tls_config(tls_config);

    connect(config, server)
        .await
        .expect("TlsConfig::hostname was used instead of the URL host");
}

#[tokio::test]
async fn mqtt_client_presents_its_stored_tls_config_over_wss() {
    let (addr, server) = wss_server(server_config(
        certs(SERVER_CERT),
        key(SERVER_KEY),
        true,
        &[],
    ))
    .await;
    let client = MqttClient::with_options(
        ConnectOptions::new("wss-stored-tls").with_automatic_reconnect(false),
    );
    client
        .set_tls_config(
            Some(std::fs::read(CLIENT_CERT).expect("read client cert")),
            Some(std::fs::read(CLIENT_KEY).expect("read client key")),
            Some(std::fs::read(CA).expect("read CA")),
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
        .expect("server saw no completed handshake: the stored TLS config was not used");
    assert!(handshake.client_presented_cert);
    connecting.abort();
}

#[tokio::test]
async fn mqtt_client_does_not_offer_its_stored_alpn_over_wss() {
    // Like this crate's wss:// listener, the server advertises only http/1.1,
    // so a client offering an MQTT protocol would get NoApplicationProtocol.
    let (addr, server) = wss_server(server_config(
        certs(SERVER_CERT),
        key(SERVER_KEY),
        false,
        &[b"http/1.1"],
    ))
    .await;
    let client = MqttClient::with_options(
        ConnectOptions::new("wss-stored-alpn").with_automatic_reconnect(false),
    );

    // connect_with_tls stores its configuration before dialing; a closed port
    // makes it fail at once, leaving that configuration stored.
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("pick a port");
    let mut stored = TlsConfig::new(closed, "127.0.0.1").with_alpn_protocols(&["mqtt"]);
    stored.load_ca_cert_pem(CA).expect("load CA");
    client
        .connect_with_tls(stored)
        .await
        .expect_err("nothing listens on the closed port");

    let connecting = tokio::spawn(async move {
        let _ = client.connect(&format!("wss://{addr}/mqtt")).await;
    });
    let handshake = timeout(WAIT, server)
        .await
        .expect("server timed out")
        .expect("server saw no completed handshake: the stored ALPN was offered");
    assert_eq!(handshake.alpn, None);
    connecting.abort();
}
