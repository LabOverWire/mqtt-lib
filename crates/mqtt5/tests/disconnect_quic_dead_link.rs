#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::config::{BrokerConfig, QuicConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient, MqttError};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

const ANSWER_DELAY: Duration = Duration::from_millis(1000);

async fn start_broker() -> (
    SocketAddr,
    Vec<u8>,
    tokio::task::JoinHandle<()>,
    tempfile::TempDir,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cert.pem"), generated.cert.pem()).unwrap();
    std::fs::write(
        dir.path().join("key.pem"),
        generated.signing_key.serialize_pem(),
    )
    .unwrap();
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_quic(
            QuicConfig::new(dir.path().join("cert.pem"), dir.path().join("key.pem"))
                .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap()),
        );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let quic = broker.quic_local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    let running = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&v| v).await.unwrap();
    (quic, generated.cert.pem().into_bytes(), running, dir)
}

async fn severable_relay(target: SocketAddr, severed: Arc<AtomicBool>) -> SocketAddr {
    let front = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let front_addr = front.local_addr().unwrap();
    let back = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    back.connect(target).await.unwrap();
    let peer: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
    {
        let front = front.clone();
        let back = back.clone();
        let peer = peer.clone();
        let severed = severed.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            while let Ok(len) = back.recv(&mut buf).await {
                let datagram = buf[..len].to_vec();
                let front = front.clone();
                let to = *peer.lock().await;
                let severed = severed.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(ANSWER_DELAY).await;
                    if severed.load(Ordering::SeqCst) {
                        return;
                    }
                    if let Some(to) = to {
                        let _ = front.send_to(&datagram, to).await;
                    }
                });
            }
        });
    }
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65535];
        while let Ok((len, from)) = front.recv_from(&mut buf).await {
            if severed.load(Ordering::SeqCst) {
                continue;
            }
            *peer.lock().await = Some(from);
            let _ = back.send(&buf[..len]).await;
        }
    });
    front_addr
}

#[tokio::test]
async fn disconnect_during_connack_on_a_severed_quic_link_returns_within_about_one_second() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("cancel_quic_dead_link=info")
        .try_init();
    let (quic, cert, running, dir) = start_broker().await;
    let severed = Arc::new(AtomicBool::new(false));
    let relay = severable_relay(quic, severed.clone()).await;
    let client = MqttClient::with_options(
        ConnectOptions::new("cancel-severed").with_automatic_reconnect(false),
    );
    client.set_tls_config(None, None, Some(cert)).await;
    client.set_quic_server_name("127.0.0.1", "localhost").await;
    let connecting = client.clone();
    let url = format!("quics://127.0.0.1:{}", relay.port());
    let connect = tokio::spawn(async move { connecting.connect(&url).await });
    tokio::time::sleep(ANSWER_DELAY + ANSWER_DELAY / 2).await;
    severed.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let disconnected = client.disconnect().await;
    let took = started.elapsed();
    let connected = connect.await.unwrap();
    tracing::info!(?took, ?disconnected, ?connected, "severed link");
    assert!(
        matches!(disconnected, Err(MqttError::NotConnected)),
        "disconnect {disconnected:?}"
    );
    assert!(connected.is_err(), "connect {connected:?}");
    assert!(
        took < Duration::from_millis(1500),
        "disconnect took {took:?}"
    );
    assert!(!client.is_connected().await);
    running.abort();
    drop(dir);
}
