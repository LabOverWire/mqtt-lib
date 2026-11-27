#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::config::{BrokerConfig, QuicConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::Mutex;

const ANSWER_DELAY: Duration = Duration::from_millis(1000);

#[tokio::test]
async fn tcp_cancelled_connect_does_not_dial_the_next_address() {
    let v4 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = v4.local_addr().unwrap().port();
    let v6 = TcpListener::bind(("::1", port)).await.unwrap();
    let client =
        MqttClient::with_options(ConnectOptions::new("cancel-tcp").with_automatic_reconnect(false));
    let connecting = client.clone();
    let url = format!("mqtt://localhost:{port}");
    let connect = tokio::spawn(async move { connecting.connect(&url).await });
    let (first, other) = tokio::select! {
        accepted = v4.accept() => (accepted.unwrap().0, v6),
        accepted = v6.accept() => (accepted.unwrap().0, v4),
    };
    let mut first = first;
    let mut buffer = [0u8; 1024];
    assert!(first.read(&mut buffer).await.unwrap() > 0);
    let disconnected = tokio::time::timeout(Duration::from_secs(2), client.disconnect())
        .await
        .unwrap();
    let connected = connect.await.unwrap();
    let dialed_again = tokio::time::timeout(Duration::from_secs(1), other.accept())
        .await
        .is_ok();
    assert!(
        !dialed_again,
        "after disconnect {disconnected:?} the cancelled connect ({connected:?}) dialed the other address"
    );
}

async fn quic_broker() -> (
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

async fn answer_delaying_relay(target: SocketAddr) -> SocketAddr {
    let front = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let front_addr = front.local_addr().unwrap();
    let back = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    back.connect(target).await.unwrap();
    let peer: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
    {
        let front = front.clone();
        let back = back.clone();
        let peer = peer.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            while let Ok(len) = back.recv(&mut buf).await {
                let datagram = buf[..len].to_vec();
                let front = front.clone();
                let to = *peer.lock().await;
                tokio::spawn(async move {
                    tokio::time::sleep(ANSWER_DELAY).await;
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
            *peer.lock().await = Some(from);
            let _ = back.send(&buf[..len]).await;
        }
    });
    front_addr
}

#[tokio::test]
async fn quic_cancelled_connect_returns_without_waiting_for_another_address() {
    cancel_quic_connect_to("localhost").await;
}

#[tokio::test]
async fn control_quic_cancelled_connect_to_one_address_returns_promptly() {
    cancel_quic_connect_to("127.0.0.1").await;
}

async fn cancel_quic_connect_to(host: &str) {
    let (quic, cert, running, dir) = quic_broker().await;
    let relay = answer_delaying_relay(quic).await;
    let client = MqttClient::with_options(
        ConnectOptions::new("cancel-quic").with_automatic_reconnect(false),
    );
    client.set_tls_config(None, None, Some(cert)).await;
    client
        .set_quic_connect_timeout(Duration::from_secs(8))
        .await;
    let connecting = client.clone();
    client.set_quic_server_name(host, "localhost").await;
    let url = format!("quics://{host}:{}", relay.port());
    let connect = tokio::spawn(async move { connecting.connect(&url).await });
    tokio::time::sleep(ANSWER_DELAY + ANSWER_DELAY / 2).await;
    let started = Instant::now();
    let disconnected = client.disconnect().await;
    let disconnect_took = started.elapsed();
    let connected = connect.await.unwrap();
    let connect_took = started.elapsed();
    running.abort();
    drop(dir);
    assert!(
        connect_took < Duration::from_secs(3),
        "disconnect {disconnected:?} after {disconnect_took:?}, connect {connected:?} after {connect_took:?}"
    );
}
