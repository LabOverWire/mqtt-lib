#![cfg(all(
    feature = "broker",
    feature = "transport-websocket",
    feature = "transport-quic"
))]

use mqtt5::broker::auth_mechanisms::{JwksCache, JwksEndpointConfig};
use mqtt5::broker::config::{
    BrokerConfig, QuicConfig, StorageBackend, StorageConfig, TlsConfig as BrokerTlsConfig,
    WebSocketConfig as BrokerWebSocketConfig,
};
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use rustls::crypto::CryptoProvider;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::TcpListener;

struct Pki {
    ca_pem: Vec<u8>,
    client_cert_pem: Vec<u8>,
    client_key_pem: Vec<u8>,
    dir: tempfile::TempDir,
}

impl Pki {
    fn generate() -> Self {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);
        let server_key = KeyPair::generate().unwrap();
        let server = CertificateParams::new(vec!["localhost".to_string()])
            .unwrap()
            .signed_by(&server_key, &issuer)
            .unwrap();
        let client_key = KeyPair::generate().unwrap();
        let client = CertificateParams::new(vec!["provider-client".to_string()])
            .unwrap()
            .signed_by(&client_key, &issuer)
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ca.pem"), ca.pem()).unwrap();
        std::fs::write(dir.path().join("server.pem"), server.pem()).unwrap();
        std::fs::write(dir.path().join("server.key"), server_key.serialize_pem()).unwrap();
        Self {
            ca_pem: ca.pem().into_bytes(),
            client_cert_pem: client.pem().into_bytes(),
            client_key_pem: client_key.serialize_pem().into_bytes(),
            dir,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn broker_tls(&self) -> BrokerTlsConfig {
        BrokerTlsConfig::new(self.path("server.pem"), self.path("server.key"))
            .with_ca_file(self.path("ca.pem"))
            .with_require_client_cert(true)
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
    }

    async fn client(&self, id: &str) -> MqttClient {
        let client =
            MqttClient::with_options(ConnectOptions::new(id).with_automatic_reconnect(false));
        client
            .set_tls_config(
                Some(self.client_cert_pem.clone()),
                Some(self.client_key_pem.clone()),
                Some(self.ca_pem.clone()),
            )
            .await
            .unwrap();
        client
    }
}

fn assert_no_process_provider() {
    assert!(
        CryptoProvider::get_default().is_none(),
        "a process-level CryptoProvider is installed, so this test cannot show the crate works without one"
    );
}

fn base() -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
}

async fn run(mut broker: MqttBroker) -> tokio::task::JoinHandle<()> {
    let mut ready = broker.ready_receiver();
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&v| v).await.unwrap();
    handle
}

async fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn mutual_tls_works_without_a_process_level_provider() {
    assert_no_process_provider();
    let pki = Pki::generate();
    let broker = MqttBroker::with_config(base().with_tls(pki.broker_tls()))
        .await
        .unwrap();
    let port = broker.tls_local_addr().unwrap().port();
    let handle = run(broker).await;
    let client = pki.client("provider-mqtts").await;
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        client.connect(&format!("mqtts://localhost:{port}")),
    )
    .await
    .unwrap();
    handle.abort();
    assert!(outcome.is_ok(), "mqtts connect failed: {outcome:?}");
}

#[tokio::test]
async fn mutual_tls_websocket_works_without_a_process_level_provider() {
    assert_no_process_provider();
    let pki = Pki::generate();
    let port = free_port().await;
    let config = base().with_tls(pki.broker_tls()).with_websocket_tls(
        BrokerWebSocketConfig::new()
            .with_bind_addresses(vec![format!("127.0.0.1:{port}").parse().unwrap()])
            .with_path("/mqtt".to_string()),
    );
    let handle = run(MqttBroker::with_config(config).await.unwrap()).await;
    let client = pki.client("provider-wss").await;
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        client.connect(&format!("wss://localhost:{port}/mqtt")),
    )
    .await
    .unwrap();
    handle.abort();
    assert!(outcome.is_ok(), "wss connect failed: {outcome:?}");
}

#[tokio::test]
async fn websocket_without_a_tls_config_returns_an_error_without_a_process_level_provider() {
    assert_no_process_provider();
    let pki = Pki::generate();
    let port = free_port().await;
    let config = base().with_tls(pki.broker_tls()).with_websocket_tls(
        BrokerWebSocketConfig::new()
            .with_bind_addresses(vec![format!("127.0.0.1:{port}").parse().unwrap()])
            .with_path("/mqtt".to_string()),
    );
    let handle = run(MqttBroker::with_config(config).await.unwrap()).await;
    let client = MqttClient::with_options(
        ConnectOptions::new("provider-wss-native").with_automatic_reconnect(false),
    );
    let outcome = tokio::spawn(async move {
        tokio::time::timeout(
            Duration::from_secs(5),
            client.connect(&format!("wss://localhost:{port}/mqtt")),
        )
        .await
    })
    .await;
    handle.abort();
    let returned = outcome.expect("wss connect without a TLS configuration panicked");
    assert!(
        matches!(returned, Ok(Err(_))),
        "a server signed by an unknown CA was accepted: {returned:?}"
    );
}

#[tokio::test]
async fn mutual_tls_quic_works_without_a_process_level_provider() {
    assert_no_process_provider();
    let pki = Pki::generate();
    let config = base().with_quic(
        QuicConfig::new(pki.path("server.pem"), pki.path("server.key"))
            .with_ca_file(pki.path("ca.pem"))
            .with_require_client_cert(true)
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap()),
    );
    let broker = MqttBroker::with_config(config).await.unwrap();
    let port = broker.quic_local_addr().unwrap().port();
    let handle = run(broker).await;
    let client = pki.client("provider-quics").await;
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        client.connect(&format!("quics://localhost:{port}")),
    )
    .await
    .unwrap();
    handle.abort();
    assert!(outcome.is_ok(), "quics connect failed: {outcome:?}");
}

#[tokio::test]
async fn jwks_fetch_works_without_a_process_level_provider() {
    assert_no_process_provider();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepting = tokio::spawn(async move {
        let _ = listener.accept().await;
    });
    let mut cache = JwksCache::new();
    cache.add_endpoint(JwksEndpointConfig {
        uri: format!("https://localhost:{port}/jwks"),
        issuer: "provider-issuer".to_string(),
        request_timeout: Duration::from_secs(2),
        ..Default::default()
    });
    let fetched = tokio::spawn(async move { cache.initial_fetch().await.map(|()| cache) }).await;
    accepting.abort();
    let cache = fetched
        .expect("a JWKS fetch panicked")
        .expect("initial_fetch failed");
    assert!(cache
        .get_keys_for_issuer("provider-issuer")
        .await
        .is_empty());
}
