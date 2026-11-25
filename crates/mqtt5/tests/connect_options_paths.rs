#![cfg(feature = "broker")]
use mqtt5::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult};
use mqtt5::broker::config::{
    BrokerConfig, StorageBackend, StorageConfig, TlsConfig as BrokerTlsConfig,
};
use mqtt5::broker::MqttBroker;
use mqtt5::client::{AuthHandler, AuthResponse};
use mqtt5::error::Result as MqttResult;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::transport::tls::TlsConfig;
use mqtt5::{ConnectOptions, MqttClient};
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const CONNACK: [u8; 5] = [0x20, 3, 0, 0, 0];

fn certs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_certs")
}

fn trusted_ca() -> Vec<u8> {
    std::fs::read(certs().join("ca.pem")).unwrap()
}

async fn tls_broker(provider: Arc<dyn AuthProvider>) -> (u16, tokio::task::JoinHandle<()>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls = BrokerTlsConfig::new(certs().join("server.pem"), certs().join("server.key"))
        .with_require_client_cert(false)
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap());
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        })
        .with_tls(tls);
    let mut broker = MqttBroker::with_config(config)
        .await
        .unwrap()
        .with_auth_provider(provider);
    let port = broker.tls_local_addr().unwrap().port();
    let mut ready = broker.ready_receiver();
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&up| up).await.unwrap();
    (port, handle)
}

fn client_tls(port: u16) -> TlsConfig {
    let mut tls = TlsConfig::new(format!("127.0.0.1:{port}").parse().unwrap(), "localhost")
        .with_system_roots(false);
    tls.load_ca_cert_pem_bytes(&trusted_ca()).unwrap();
    tls
}

#[derive(Default)]
struct Recording {
    client_ids: Mutex<Vec<String>>,
}

impl AuthProvider for Recording {
    fn authenticate<'a>(
        &'a self,
        connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = MqttResult<AuthResult>> + Send + 'a>> {
        self.client_ids
            .lock()
            .unwrap()
            .push(connect.client_id.clone());
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
}

struct Challenging;

impl AuthProvider for Challenging {
    fn authenticate<'a>(
        &'a self,
        _connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = MqttResult<AuthResult>> + Send + 'a>> {
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
    ) -> Pin<Box<dyn Future<Output = MqttResult<EnhancedAuthResult>> + Send + 'a>> {
        let method = auth_method.to_string();
        let answered = auth_data == Some(b"answer".as_slice());
        Box::pin(async move {
            Ok(if answered {
                EnhancedAuthResult::success(method)
            } else {
                EnhancedAuthResult::continue_auth(method, Some(b"challenge".to_vec()))
            })
        })
    }
}

struct Answering;

impl AuthHandler for Answering {
    fn handle_challenge<'a>(
        &'a self,
        _auth_method: &'a str,
        _challenge_data: Option<&'a [u8]>,
    ) -> Pin<Box<dyn Future<Output = MqttResult<AuthResponse>> + Send + 'a>> {
        Box::pin(async move { Ok(AuthResponse::Continue(b"answer".to_vec())) })
    }
}

async fn recording_server(connections: usize) -> (String, mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let (sent, received) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        for served in 1..=connections {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = [0u8; 2048];
            let read = stream.read(&mut buffer).await.unwrap_or(0);
            let _ = sent.send(String::from_utf8_lossy(&buffer[..read]).into_owned());
            let _ = stream.write_all(&CONNACK).await;
            if served == connections {
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    });
    (url, received)
}

async fn next_connect(received: &mut mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn connect_with_options_sends_the_client_id_in_its_options() {
    let (url, mut received) = recording_server(1).await;
    let client = MqttClient::new("id-from-new");
    let options = ConnectOptions::new("id-from-options").with_automatic_reconnect(false);
    client.connect_with_options(&url, options).await.unwrap();
    let connect = next_connect(&mut received).await;
    let reported = client.client_id().await;
    let _ = client.disconnect().await;
    assert!(
        connect.contains("id-from-options") && !connect.contains("id-from-new"),
        "CONNECT carried: {connect}"
    );
    assert_eq!(reported, "id-from-options");
}

#[tokio::test]
async fn a_reconnection_sends_the_client_id_given_to_connect_with_options() {
    let (url, mut received) = recording_server(2).await;
    let client = MqttClient::new("id-from-new");
    let options = ConnectOptions::new("id-from-options")
        .with_reconnect_delay(Duration::from_millis(10), Duration::from_millis(50));
    client.connect_with_options(&url, options).await.unwrap();
    let first = next_connect(&mut received).await;
    let second = next_connect(&mut received).await;
    let _ = client.disconnect().await;
    assert!(first.contains("id-from-options"), "first CONNECT: {first}");
    assert!(
        second.contains("id-from-options") && !second.contains("id-from-new"),
        "reconnection CONNECT: {second}"
    );
}

#[tokio::test]
async fn connect_with_tls_and_options_sends_the_client_id_in_its_options() {
    let recording = Arc::new(Recording::default());
    let (port, broker) = tls_broker(recording.clone()).await;
    let client = MqttClient::new("tls-id-from-new");
    let options = ConnectOptions::new("tls-id-from-options").with_automatic_reconnect(false);
    let result = client
        .connect_with_tls_and_options(client_tls(port), options)
        .await;
    let _ = client.disconnect().await;
    broker.abort();
    result.unwrap();
    let seen = recording.client_ids.lock().unwrap().clone();
    assert_eq!(seen, vec!["tls-id-from-options".to_string()]);
}

#[tokio::test]
async fn enhanced_auth_works_through_connect_with_tls_and_options() {
    let (port, broker) = tls_broker(Arc::new(Challenging)).await;
    let options = || {
        ConnectOptions::new("enhanced")
            .with_automatic_reconnect(false)
            .with_authentication_method("X")
    };
    let control = MqttClient::new("enhanced");
    control.set_auth_handler(Answering).await;
    control.set_tls_config(None, None, Some(trusted_ca())).await;
    let over_url = control
        .connect_with_options(&format!("mqtts://localhost:{port}"), options())
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());
    let _ = control.disconnect().await;
    let client = MqttClient::new("enhanced");
    client.set_auth_handler(Answering).await;
    let over_tls = client
        .connect_with_tls_and_options(client_tls(port), options())
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());
    let _ = client.disconnect().await;
    broker.abort();
    assert_eq!(over_url, Ok(()), "control over mqtts:// failed");
    assert_eq!(over_tls, Ok(()), "connect_with_tls_and_options failed");
}

#[tokio::test]
async fn connect_with_tls_and_options_refuses_deferred_ack_without_a_persistent_session() {
    let recording = Arc::new(Recording::default());
    let (port, broker) = tls_broker(recording.clone()).await;
    let invalid = || {
        ConnectOptions::new("deferred")
            .with_automatic_reconnect(false)
            .with_deferred_ack(true)
    };
    let control = MqttClient::new("deferred");
    control.set_tls_config(None, None, Some(trusted_ca())).await;
    let over_url = control
        .connect_with_options(&format!("mqtts://localhost:{port}"), invalid())
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());
    let client = MqttClient::new("deferred");
    let over_tls = client
        .connect_with_tls_and_options(client_tls(port), invalid())
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());
    let _ = client.disconnect().await;
    broker.abort();
    let refusal =
        "Configuration error: deferred_ack requires a persistent session: set clean_start(false)";
    assert_eq!(over_url, Err(refusal.to_string()));
    assert_eq!(over_tls, Err(refusal.to_string()));
    assert!(recording.client_ids.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_cancelled_connect_with_options_puts_back_the_client_id() {
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", silent.local_addr().unwrap());
    let client = MqttClient::new("id-from-new");
    let options = ConnectOptions::new("id-from-cancelled").with_automatic_reconnect(false);
    let cancelled = tokio::time::timeout(
        Duration::from_millis(100),
        client.connect_with_options(&url, options),
    )
    .await;
    assert!(cancelled.is_err(), "the connect was to be cancelled");
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(silent);
    assert_eq!(client.client_id().await, "id-from-new");
}

#[tokio::test]
async fn refused_deferred_ack_options_are_not_kept_for_the_next_connect() {
    let (url, mut received) = recording_server(1).await;
    let client = MqttClient::new("kept-options");
    let invalid = ConnectOptions::new("refused-options")
        .with_automatic_reconnect(false)
        .with_deferred_ack(true);
    assert!(client.connect_with_options(&url, invalid).await.is_err());
    let next = client.connect(&url).await;
    assert!(next.is_ok(), "connect after the refusal: {next:?}");
    let connect = next_connect(&mut received).await;
    let _ = client.disconnect().await;
    assert!(
        connect.contains("kept-options"),
        "CONNECT carried: {connect}"
    );
}
