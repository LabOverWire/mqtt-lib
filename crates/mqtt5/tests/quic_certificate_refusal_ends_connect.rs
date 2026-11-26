#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::config::{BrokerConfig, QuicConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

struct Broker {
    port: u16,
    trusted: Vec<u8>,
    running: tokio::task::JoinHandle<()>,
    silent_ipv6: Option<std::net::UdpSocket>,
}

async fn quic_broker_on_ipv4_only() -> Broker {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let trusted = certified.cert.pem().into_bytes();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cert.pem"), &trusted).unwrap();
    std::fs::write(
        dir.path().join("key.pem"),
        certified.signing_key.serialize_pem(),
    )
    .unwrap();
    let config = BrokerConfig::default()
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_quic(
            QuicConfig::new(dir.path().join("cert.pem"), dir.path().join("key.pem"))
                .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap()),
        );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let port = broker.quic_local_addr().unwrap().port();
    let mut ready = broker.ready_receiver();
    let running = tokio::spawn(async move {
        let _ = broker.run().await;
        drop(dir);
    });
    ready.wait_for(|&v| v).await.unwrap();
    let silent_ipv6 = std::net::UdpSocket::bind(("::1", port)).ok();
    Broker {
        port,
        trusted,
        running,
        silent_ipv6,
    }
}

fn other_ca() -> Vec<u8> {
    rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .unwrap()
        .cert
        .pem()
        .into_bytes()
}

async fn connect_to_localhost(port: u16, ca: Vec<u8>) -> (mqtt5::error::Result<()>, Duration) {
    let id = format!("quic-cert-refusal-{}", std::process::id());
    let client = MqttClient::new(&id);
    client.set_tls_config(None, None, Some(ca)).await;
    let started = Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        client.connect_with_options(
            &format!("quics://localhost:{port}"),
            ConnectOptions::new(id).with_automatic_reconnect(false),
        ),
    )
    .await
    .expect("connect did not finish in 20 s")
    .map(|_| ());
    let elapsed = started.elapsed();
    let _ = client.disconnect().await;
    (outcome, elapsed)
}

#[tokio::test]
async fn control_the_trusted_certificate_connects_past_a_silent_address() {
    let broker = quic_broker_on_ipv4_only().await;
    let (outcome, elapsed) = connect_to_localhost(broker.port, broker.trusted.clone()).await;
    broker.running.abort();
    drop(broker.silent_ipv6);
    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
}

#[tokio::test]
async fn a_refused_certificate_ends_the_connect_without_waiting_for_a_silent_address() {
    let broker = quic_broker_on_ipv4_only().await;
    let (outcome, elapsed) = connect_to_localhost(broker.port, other_ca()).await;
    broker.running.abort();
    drop(broker.silent_ipv6);
    assert!(outcome.is_err(), "{outcome:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "took {elapsed:?}: {outcome:?}"
    );
}
