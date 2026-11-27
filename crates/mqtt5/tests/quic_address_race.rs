#![cfg(feature = "transport-quic")]

use mqtt5::{ConnectOptions, MqttClient};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

const CONNACK_ACCEPTED: [u8; 5] = [0x20, 0x03, 0x00, 0x00, 0x00];

#[derive(Default)]
struct Seen {
    connections: AtomicUsize,
    connects: AtomicUsize,
    closed_without_connect: AtomicUsize,
}

#[derive(Clone, Copy)]
struct Behaviour {
    handshake_delay: Duration,
    idle_timeout: Duration,
    connack_delay: Option<Duration>,
}

fn server_config(idle_timeout: Duration) -> quinn::ServerConfig {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert = rustls_pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key = rustls_pki_types::PrivateKeyDer::Pkcs8(rustls_pki_types::PrivatePkcs8KeyDer::from(
        certified.signing_key.serialize_der(),
    ));
    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    crypto.alpn_protocols = vec![b"mqtt".to_vec()];
    let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(crypto).unwrap();
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(quinn::IdleTimeout::try_from(idle_timeout).unwrap()));
    config.transport_config(Arc::new(transport));
    config
}

fn serve(addr: SocketAddr, behaviour: Behaviour, seen: Arc<Seen>) -> tokio::task::JoinHandle<()> {
    let endpoint = quinn::Endpoint::server(server_config(behaviour.idle_timeout), addr).unwrap();
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                tokio::time::sleep(behaviour.handshake_delay).await;
                let Ok(connection) = incoming.await else {
                    return;
                };
                seen.connections.fetch_add(1, Ordering::SeqCst);
                let Ok((mut send, mut recv)) = connection.accept_bi().await else {
                    seen.closed_without_connect.fetch_add(1, Ordering::SeqCst);
                    return;
                };
                let mut buf = [0u8; 512];
                if !matches!(recv.read(&mut buf).await, Ok(Some(n)) if n > 0) {
                    return;
                }
                seen.connects.fetch_add(1, Ordering::SeqCst);
                if let Some(delay) = behaviour.connack_delay {
                    tokio::time::sleep(delay).await;
                    let _ = send.write_all(&CONNACK_ACCEPTED).await;
                    let _ = send.flush().await;
                }
                connection.closed().await;
            });
        }
    })
}

fn bind_pair() -> (SocketAddr, SocketAddr) {
    for _ in 0..50 {
        let v4 = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = v4.local_addr().unwrap().port();
        if std::net::UdpSocket::bind(("::1", port)).is_ok() {
            return (
                SocketAddr::from(([127, 0, 0, 1], port)),
                SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)),
            );
        }
    }
    panic!("no port free on both loopbacks");
}

async fn localhost_resolves_v6_first(port: u16) {
    let resolved: Vec<SocketAddr> = tokio::net::lookup_host(("localhost", port))
        .await
        .unwrap()
        .collect();
    assert!(
        resolved.first().is_some_and(SocketAddr::is_ipv6)
            && resolved.iter().any(SocketAddr::is_ipv4),
        "precondition: localhost resolves to ::1 then 127.0.0.1, got {resolved:?}"
    );
}

#[tokio::test]
async fn hung_first_address_does_not_spoil_the_one_opened_beside_it() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (v4, v6) = bind_pair();
    localhost_resolves_v6_first(v4.port()).await;
    let hung = Arc::new(Seen::default());
    let good = Arc::new(Seen::default());
    let hung_server = serve(
        v6,
        Behaviour {
            handshake_delay: Duration::ZERO,
            idle_timeout: Duration::from_secs(60),
            connack_delay: None,
        },
        hung.clone(),
    );
    let good_server = serve(
        v4,
        Behaviour {
            handshake_delay: Duration::from_millis(300),
            idle_timeout: Duration::from_secs(3),
            connack_delay: Some(Duration::ZERO),
        },
        good.clone(),
    );

    let client = MqttClient::new("address-race-hung");
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        client.connect_with_options(
            &format!("quic://localhost:{}", v4.port()),
            ConnectOptions::new("address-race-hung")
                .with_automatic_reconnect(false)
                .with_connect_packet_timeout(Duration::from_secs(6)),
        ),
    )
    .await;
    let elapsed = started.elapsed();
    let _ = client.disconnect().await;
    hung_server.abort();
    good_server.abort();
    assert!(
        matches!(result, Ok(Ok(_))),
        "connect result {result:?} after {elapsed:?}; hung saw {} connections {} CONNECTs; good saw {} connections {} CONNECTs",
        hung.connections.load(Ordering::SeqCst),
        hung.connects.load(Ordering::SeqCst),
        good.connections.load(Ordering::SeqCst),
        good.connects.load(Ordering::SeqCst),
    );
}

#[tokio::test]
async fn losing_quic_connection_is_closed_promptly_and_sends_no_connect() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (v4, v6) = bind_pair();
    localhost_resolves_v6_first(v4.port()).await;
    let first = Arc::new(Seen::default());
    let second = Arc::new(Seen::default());
    let behaviour = Behaviour {
        handshake_delay: Duration::ZERO,
        idle_timeout: Duration::from_secs(60),
        connack_delay: Some(Duration::from_millis(500)),
    };
    let first_server = serve(v6, behaviour, first.clone());
    let second_server = serve(v4, behaviour, second.clone());

    let client = MqttClient::new("address-race-loser");
    let result = client
        .connect_with_options(
            &format!("quic://localhost:{}", v4.port()),
            ConnectOptions::new("address-race-loser").with_automatic_reconnect(false),
        )
        .await;
    assert!(result.is_ok(), "connect result {result:?}");
    tokio::time::sleep(Duration::from_secs(3)).await;
    let connections =
        first.connections.load(Ordering::SeqCst) + second.connections.load(Ordering::SeqCst);
    let connects = first.connects.load(Ordering::SeqCst) + second.connects.load(Ordering::SeqCst);
    let closed = first.closed_without_connect.load(Ordering::SeqCst)
        + second.closed_without_connect.load(Ordering::SeqCst);
    let _ = client.disconnect().await;
    first_server.abort();
    second_server.abort();
    assert_eq!(connections, 2, "both addresses completed a handshake");
    assert_eq!(connects, 1, "CONNECTs sent across both addresses");
    assert_eq!(
        connections - connects,
        closed,
        "every QUIC connection that sent no CONNECT is closed within 3 s: {connections} connections, {connects} CONNECTs, {closed} closed"
    );
}
