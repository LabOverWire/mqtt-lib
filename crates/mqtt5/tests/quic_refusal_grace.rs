#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::auth::PasswordAuthProvider;
use mqtt5::broker::config::{AuthConfig, BrokerConfig, QuicConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::error::MqttError;
use mqtt5::{ConnectOptions, MqttClient};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

fn runtime_password() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("pw-{}-{nanos}", std::process::id())
}

struct Certified {
    cert_pem: Vec<u8>,
    dir: tempfile::TempDir,
}

fn certified() -> Certified {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let generated =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cert.pem"), generated.cert.pem()).unwrap();
    std::fs::write(
        dir.path().join("key.pem"),
        generated.signing_key.serialize_pem(),
    )
    .unwrap();
    Certified {
        cert_pem: generated.cert.pem().into_bytes(),
        dir,
    }
}

async fn quic_broker(
    certified: &Certified,
    bind: &str,
    anonymous: bool,
) -> (u16, tokio::task::JoinHandle<()>) {
    let auth = if anonymous {
        AuthConfig::new()
    } else {
        let path = certified.dir.path().join("passwords.txt");
        std::fs::write(
            &path,
            format!(
                "someone:{}\n",
                PasswordAuthProvider::hash_password(&runtime_password()).unwrap()
            ),
        )
        .unwrap();
        AuthConfig::new()
            .with_password_file(path)
            .with_allow_anonymous(false)
    };
    let config = BrokerConfig::default()
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_auth(auth)
        .with_quic(
            QuicConfig::new(
                certified.dir.path().join("cert.pem"),
                certified.dir.path().join("key.pem"),
            )
            .with_bind_address(bind.parse::<SocketAddr>().unwrap()),
        );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let port = broker.quic_local_addr().unwrap().port();
    let mut ready = broker.ready_receiver();
    let running = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&v| v).await.unwrap();
    (port, running)
}

async fn ipv6_relay(
    front_port: u16,
    target_port: u16,
    delay: Duration,
) -> tokio::task::JoinHandle<()> {
    let front = Arc::new(UdpSocket::bind(("::1", front_port)).await.unwrap());
    let back = Arc::new(UdpSocket::bind("[::1]:0").await.unwrap());
    back.connect(("::1", target_port)).await.unwrap();
    let peer: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
    let replies = {
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
                    tokio::time::sleep(delay).await;
                    if let Some(to) = to {
                        let _ = front.send_to(&datagram, to).await;
                    }
                });
            }
        })
    };
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65535];
        while let Ok((len, from)) = front.recv_from(&mut buf).await {
            *peer.lock().await = Some(from);
            let datagram = buf[..len].to_vec();
            let back = back.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = back.send(&datagram).await;
            });
        }
        replies.abort();
    })
}

async fn connect(url: &str, ca: Vec<u8>, id: &str) -> (mqtt5::error::Result<()>, Duration) {
    let client = MqttClient::new(id);
    client.set_tls_config(None, None, Some(ca)).await;
    let started = Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        client.connect_with_options(url, ConnectOptions::new(id).with_automatic_reconnect(false)),
    )
    .await
    .expect("connect did not finish in 20 s")
    .map(|_| ());
    let elapsed = started.elapsed();
    let _ = client.disconnect().await;
    (outcome, elapsed)
}

fn is_tls_refusal(outcome: &mqtt5::error::Result<()>) -> bool {
    matches!(outcome, Err(MqttError::ConnectionError(m)) if m.contains("QUIC TLS handshake refused"))
}

fn is_refused_for_good(outcome: &mqtt5::error::Result<()>) -> bool {
    matches!(
        outcome,
        Err(MqttError::ConnectionRefused(
            mqtt5::protocol::v5::reason_codes::ReasonCode::NotAuthorized
                | mqtt5::protocol::v5::reason_codes::ReasonCode::BadUsernameOrPassword
        ))
    )
}

#[tokio::test]
async fn the_grace_after_a_refusal_lasts_about_two_seconds_past_a_silent_address() {
    let served = certified();
    let (port, running) = quic_broker(&served, "127.0.0.1:0", true).await;
    let silent = std::net::UdpSocket::bind(("::1", port)).unwrap();
    let (outcome, elapsed) = connect(
        &format!("quics://localhost:{port}"),
        certified().cert_pem,
        "review373b-silent",
    )
    .await;
    running.abort();
    drop(silent);
    assert!(is_tls_refusal(&outcome), "{outcome:?}");
    assert!(
        elapsed >= Duration::from_millis(1900) && elapsed < Duration::from_secs(4),
        "took {elapsed:?}"
    );
}

#[tokio::test]
async fn a_single_address_refusal_returns_at_once() {
    let served = certified();
    let (port, running) = quic_broker(&served, "127.0.0.1:0", true).await;
    let (outcome, elapsed) = connect(
        &format!("quics://127.0.0.1:{port}"),
        certified().cert_pem,
        "review373b-single",
    )
    .await;
    running.abort();
    assert!(is_tls_refusal(&outcome), "{outcome:?}");
    assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");
}

#[tokio::test]
async fn an_address_opening_within_the_grace_whose_connect_is_refused_returns_that_refusal() {
    let untrusted = certified();
    let (port, untrusted_running) = quic_broker(&untrusted, "127.0.0.1:0", true).await;
    let trusted = certified();
    let (target, trusted_running) = quic_broker(&trusted, "[::1]:0", false).await;
    let relay = ipv6_relay(port, target, Duration::from_millis(300)).await;
    let (outcome, elapsed) = connect(
        &format!("quics://localhost:{port}"),
        trusted.cert_pem.clone(),
        "review373b-connect-refused",
    )
    .await;
    relay.abort();
    untrusted_running.abort();
    trusted_running.abort();
    assert!(
        is_refused_for_good(&outcome),
        "{outcome:?} after {elapsed:?}"
    );
}

#[tokio::test]
async fn a_refusal_for_good_before_a_tls_refusal_still_wins() {
    let trusted = certified();
    let (port, trusted_running) = quic_broker(&trusted, "127.0.0.1:0", false).await;
    let untrusted = certified();
    let (target, untrusted_running) = quic_broker(&untrusted, "[::1]:0", true).await;
    let relay = ipv6_relay(port, target, Duration::from_millis(300)).await;
    let (outcome, elapsed) = connect(
        &format!("quics://localhost:{port}"),
        trusted.cert_pem.clone(),
        "review373b-for-good-first",
    )
    .await;
    relay.abort();
    trusted_running.abort();
    untrusted_running.abort();
    assert!(
        is_refused_for_good(&outcome),
        "{outcome:?} after {elapsed:?}"
    );
}

#[tokio::test]
async fn an_address_opening_after_the_grace_is_abandoned_for_the_refusal() {
    let untrusted = certified();
    let (port, untrusted_running) = quic_broker(&untrusted, "127.0.0.1:0", true).await;
    let trusted = certified();
    let (target, trusted_running) = quic_broker(&trusted, "[::1]:0", true).await;
    let relay = ipv6_relay(port, target, Duration::from_millis(1500)).await;
    let (outcome, elapsed) = connect(
        &format!("quics://localhost:{port}"),
        trusted.cert_pem.clone(),
        "review373b-late",
    )
    .await;
    relay.abort();
    untrusted_running.abort();
    trusted_running.abort();
    assert!(is_tls_refusal(&outcome), "{outcome:?}");
    assert!(elapsed < Duration::from_secs(4), "took {elapsed:?}");
}
