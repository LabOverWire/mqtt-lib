#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::config::{BrokerConfig, QuicConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, Message, MqttClient, WillMessage};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

const ANSWER_DELAY: Duration = Duration::from_millis(1000);

struct Broker {
    tcp: SocketAddr,
    quic: SocketAddr,
    cert: Vec<u8>,
    running: tokio::task::JoinHandle<()>,
    dir: tempfile::TempDir,
}

async fn broker() -> Broker {
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
    let tcp = broker.local_addr().unwrap();
    let quic = broker.quic_local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    let running = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&v| v).await.unwrap();
    Broker {
        tcp,
        quic,
        cert: generated.cert.pem().into_bytes(),
        running,
        dir,
    }
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

async fn will_watcher(addr: SocketAddr, topic: &str) -> (MqttClient, Arc<AtomicUsize>) {
    let watcher = MqttClient::new(format!("quic-cancel-watcher-{}", topic.replace('/', "-")));
    watcher.connect(&format!("mqtt://{addr}")).await.unwrap();
    let wills = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&wills);
    watcher
        .subscribe(topic, move |_: Message| {
            counting.fetch_add(1, Ordering::SeqCst);
        })
        .await
        .unwrap();
    (watcher, wills)
}

async fn cancel_during_connack(topic: &str, abort_instead: bool) -> (usize, String) {
    let broker = broker().await;
    let (watcher, wills) = will_watcher(broker.tcp, topic).await;
    let relay = answer_delaying_relay(broker.quic).await;
    let id = format!("quic-cancel-{}", topic.replace('/', "-"));
    let client = MqttClient::with_options(
        ConnectOptions::new(&id)
            .with_will(WillMessage::new(topic, b"gone".to_vec()))
            .with_automatic_reconnect(false),
    );
    client
        .set_tls_config(None, None, Some(broker.cert.clone()))
        .await;
    client.set_quic_server_name("127.0.0.1", "localhost").await;
    let connecting = client.clone();
    let url = format!("quics://127.0.0.1:{}", relay.port());
    let connect = tokio::spawn(async move { connecting.connect(&url).await });
    tokio::time::sleep(ANSWER_DELAY + ANSWER_DELAY / 2).await;
    let outcome = if abort_instead {
        connect.abort();
        let _ = connect.await;
        "aborted".to_string()
    } else {
        let started = std::time::Instant::now();
        let disconnected = client.disconnect().await;
        let took = started.elapsed();
        let connected = connect.await.unwrap();
        assert!(took < Duration::from_secs(3), "disconnect took {took:?}");
        format!("disconnect {disconnected:?} after {took:?}, connect {connected:?}")
    };
    tokio::time::sleep(Duration::from_secs(8)).await;
    let received = wills.load(Ordering::SeqCst);
    watcher.disconnect().await.ok();
    broker.running.abort();
    drop(broker.dir);
    (received, outcome)
}

#[tokio::test]
async fn quic_disconnect_while_connack_is_in_flight_does_not_publish_the_will() {
    let (received, outcome) = cancel_during_connack("quic-cancel/will/a", false).await;
    assert_eq!(received, 0, "{outcome}");
}

#[tokio::test]
async fn control_quic_connect_dropped_while_connack_is_in_flight_publishes_the_will() {
    let (received, outcome) = cancel_during_connack("quic-cancel/will/b", true).await;
    assert_eq!(received, 1, "{outcome}");
}
