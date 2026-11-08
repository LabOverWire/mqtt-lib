#![cfg(feature = "broker")]

use mqtt5::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig, TlsConfig as BrokerTls};
use mqtt5::broker::MqttBroker;
use mqtt5::client::{AuthHandler, AuthResponse};
use mqtt5::error::Result;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::time::Duration;
use mqtt5::transport::tls::TlsConfig;
use mqtt5::{ConnectOptions, MqttClient};
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;

fn certs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_certs")
}

struct Challenge;

impl AuthProvider for Challenge {
    fn authenticate<'a>(
        &'a self,
        _connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async move { Ok(AuthResult::success()) })
    }
    fn authorize_publish<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { true })
    }
    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { true })
    }
    fn supports_enhanced_auth(&self) -> bool {
        true
    }
    fn authenticate_enhanced<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        _client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        let method = auth_method.to_string();
        Box::pin(async move {
            match auth_data {
                None => Ok(EnhancedAuthResult::continue_auth(
                    method,
                    Some(b"chal".to_vec()),
                )),
                Some(b"resp") => Ok(EnhancedAuthResult::success(method)),
                Some(_) => Ok(EnhancedAuthResult::fail(method, ReasonCode::NotAuthorized)),
            }
        })
    }
}

struct Answer;

impl AuthHandler for Answer {
    fn handle_challenge<'a>(
        &'a self,
        _auth_method: &'a str,
        _challenge_data: Option<&'a [u8]>,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResponse>> + Send + 'a>> {
        Box::pin(async move { Ok(AuthResponse::Continue(b"resp".to_vec())) })
    }
}

async fn tls_broker_with_challenge() -> (SocketAddr, SocketAddr, tokio::task::JoinHandle<()>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let any: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let config = BrokerConfig::default()
        .with_bind_address(any)
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        })
        .with_tls(
            BrokerTls::new(certs().join("server.pem"), certs().join("server.key"))
                .with_require_client_cert(false)
                .with_bind_address(any),
        );
    let mut broker = MqttBroker::with_config(config)
        .await
        .unwrap()
        .with_auth_provider(Arc::new(Challenge));
    let tls = broker.tls_local_addr().unwrap();
    let plain = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&v| v).await.unwrap();
    (tls, plain, handle)
}

fn client_tls(addr: SocketAddr) -> TlsConfig {
    let mut tls = TlsConfig::new(addr, "localhost");
    tls.load_ca_cert_pem(certs().join("ca.pem").to_str().unwrap())
        .unwrap();
    tls
}

#[tokio::test]
async fn enhanced_auth_over_connect_with_tls_and_options() {
    let (tls_addr, _, broker) = tls_broker_with_challenge().await;
    let client = MqttClient::new("tls-enhanced");
    client.set_auth_handler(Answer).await;
    let outcome = client
        .connect_with_tls_and_options(
            client_tls(tls_addr),
            ConnectOptions::new("tls-enhanced")
                .with_authentication_method("CHALLENGE")
                .with_automatic_reconnect(false),
        )
        .await;
    broker.abort();
    assert!(outcome.is_ok(), "{outcome:?}");
}

#[tokio::test]
async fn enhanced_auth_over_connect_with_options_control() {
    let (tls_addr, _, broker) = tls_broker_with_challenge().await;
    let client = MqttClient::new("tls-enhanced-url");
    client
        .set_tls_config(
            None,
            None,
            Some(std::fs::read(certs().join("ca.pem")).unwrap()),
        )
        .await;
    client.set_auth_handler(Answer).await;
    let outcome = client
        .connect_with_options(
            &format!("mqtts://localhost:{}", tls_addr.port()),
            ConnectOptions::new("tls-enhanced-url")
                .with_authentication_method("CHALLENGE")
                .with_automatic_reconnect(false),
        )
        .await;
    broker.abort();
    assert!(outcome.is_ok(), "{outcome:?}");
}

async fn counting_listener() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                drop(stream);
            });
        }
    });
    (addr, accepted)
}

async fn client_refused_options_after_a_reconnection(bad: ConnectOptions) -> (String, usize) {
    let dead = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let delay = Duration::from_millis(200);
    let client = MqttClient::with_options(
        ConnectOptions::new("refused-options").with_reconnect_delay(delay, delay),
    );
    client
        .connect(&format!("mqtt://{dead}"))
        .await
        .expect_err("nothing listens");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (target, accepted) = counting_listener().await;
    let error = client
        .connect_with_options(
            &format!("mqtt://{target}"),
            bad.with_reconnect_delay(delay, delay),
        )
        .await
        .expect_err("the client refuses these options");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after_connect = accepted.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let later = accepted.load(Ordering::SeqCst) - after_connect;
    let _ = client.disconnect().await;
    (format!("{error:?}"), later)
}

#[tokio::test]
async fn a_password_the_client_refuses_leaves_no_reconnection_running() {
    let (error, later) = client_refused_options_after_a_reconnection(
        ConnectOptions::new("refused-options").with_credentials("user", vec![b'p'; 70_000]),
    )
    .await;
    assert_eq!(later, 0, "a reconnection kept running after {error}");
}

#[tokio::test]
async fn a_username_the_client_refuses_leaves_no_reconnection_running() {
    let (error, later) = client_refused_options_after_a_reconnection(
        ConnectOptions::new("refused-options").with_credentials("us\0er", b"pw"),
    )
    .await;
    assert_eq!(later, 0, "a reconnection kept running after {error}");
}

#[tokio::test]
async fn a_configuration_error_leaves_no_reconnection_control() {
    let (error, later) = client_refused_options_after_a_reconnection(
        ConnectOptions::new("refused-options")
            .with_deferred_ack(true)
            .with_receive_maximum(0),
    )
    .await;
    assert_eq!(later, 0, "a reconnection kept running after {error}");
}
