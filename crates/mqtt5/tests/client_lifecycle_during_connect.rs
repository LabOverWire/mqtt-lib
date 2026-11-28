#![cfg(feature = "broker")]

use mqtt5::broker::config::{
    BrokerConfig, StorageBackend, StorageConfig, TlsConfig as BrokerTlsConfig,
};
use mqtt5::broker::MqttBroker;
use mqtt5::transport::tls::TlsConfig;
use mqtt5::{ConnectOptions, MqttClient};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

fn certs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_certs")
}

fn trusted_ca() -> Vec<u8> {
    std::fs::read(certs().join("ca.pem")).unwrap()
}

fn install_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

async fn unused_address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
}

fn reconnecting(client_id: &str) -> ConnectOptions {
    ConnectOptions::new(client_id)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(100))
}

async fn silent_server() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&accepted);
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut stream, _)) = listener.accept().await {
            counting.fetch_add(1, Ordering::SeqCst);
            let mut buffer = [0u8; 256];
            let _ =
                tokio::time::timeout(Duration::from_millis(200), stream.read(&mut buffer)).await;
            held.push(stream);
        }
    });
    (addr, accepted)
}

async fn tls_broker() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    install_provider();
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_tls(
            BrokerTlsConfig::new(certs().join("server.pem"), certs().join("server.key"))
                .with_require_client_cert(false)
                .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap()),
        );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.tls_local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&v| v).await.unwrap();
    (addr, handle)
}

async fn delaying_proxy(
    addr: SocketAddr,
    upstream: SocketAddr,
    delay: Duration,
) -> Arc<AtomicUsize> {
    let listener = TcpListener::bind(addr).await.unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((mut downstream, _)) = listener.accept().await {
            counting.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let Ok(mut up) = TcpStream::connect(upstream).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut downstream, &mut up).await;
            });
        }
    });
    accepted
}

#[tokio::test]
async fn a_disconnect_during_a_tls_reconnection_handshake_leaves_the_client_disconnected() {
    let (upstream, broker) = tls_broker().await;
    let proxy_addr = unused_address().await;
    let client = MqttClient::with_options(reconnecting("tls-disconnect-during-reconnection"));
    client
        .set_tls_config(None, None, Some(trusted_ca()))
        .await
        .unwrap();
    let url = format!("mqtts://localhost:{}", proxy_addr.port());
    client
        .connect(&url)
        .await
        .expect_err("nothing listens there yet");
    let accepted = delaying_proxy(proxy_addr, upstream, Duration::from_millis(1500)).await;
    while accepted.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let disconnected = client.disconnect().await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let connected = client.is_connected().await;
    broker.abort();
    assert!(
        !connected,
        "connected after disconnect returned {disconnected:?}"
    );
}

#[tokio::test]
async fn control_a_refused_first_tls_connect_starts_reconnection() {
    install_provider();
    let addr = unused_address().await;
    let client = MqttClient::with_options(reconnecting("refused-first-connect"));
    let outcome = client
        .connect_with_tls(
            TlsConfig::new(addr, "localhost").with_connect_timeout(Duration::from_secs(1)),
        )
        .await;
    let listener = TcpListener::bind(addr).await.unwrap();
    let retried = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .is_ok();
    client.disconnect().await.ok();
    assert!(retried, "no reconnection after {outcome:?}");
}

#[tokio::test]
async fn a_tls_handshake_timeout_on_a_first_connect_starts_reconnection() {
    install_provider();
    let (addr, accepted) = silent_server().await;
    let client = MqttClient::with_options(reconnecting("handshake-timeout-first-connect"));
    let outcome = client
        .connect_with_tls(
            TlsConfig::new(addr, "localhost").with_connect_timeout(Duration::from_secs(1)),
        )
        .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let count = accepted.load(Ordering::SeqCst);
    client.disconnect().await.ok();
    assert!(count > 1, "{count} connection after {outcome:?}");
}

#[tokio::test]
async fn control_is_connected_returns_without_a_connect() {
    let client = MqttClient::new("is-connected-control");
    let state = tokio::time::timeout(Duration::from_secs(2), client.is_connected()).await;
    assert_eq!(state, Ok(false));
}

#[tokio::test]
async fn is_connected_returns_while_connect_waits_for_connack() {
    let (addr, accepted) = silent_server().await;
    let client = MqttClient::new("is-connected-during-connack-wait");
    let connecting = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .connect_with_options(
                    &format!("mqtt://{addr}"),
                    ConnectOptions::new("is-connected-during-connack-wait")
                        .with_automatic_reconnect(false),
                )
                .await
        })
    };
    while accepted.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let state = tokio::time::timeout(Duration::from_secs(2), client.is_connected()).await;
    connecting.abort();
    assert_eq!(state, Ok(false), "is_connected waited for the CONNACK");
}
