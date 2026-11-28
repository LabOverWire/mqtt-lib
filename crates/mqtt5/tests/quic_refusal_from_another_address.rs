#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::config::{BrokerConfig, QuicConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

const RELAY_DELAY: Duration = Duration::from_millis(150);

struct Certified {
    cert_pem: Vec<u8>,
    cert_der: rustls::pki_types::CertificateDer<'static>,
    key_der: Vec<u8>,
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
        cert_der: generated.cert.der().clone(),
        key_der: generated.signing_key.serialize_der(),
        dir,
    }
}

async fn quic_broker(certified: &Certified, bind: &str) -> (u16, tokio::task::JoinHandle<()>) {
    let config = BrokerConfig::default()
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
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

fn other_protocol_server(certified: &Certified, port: u16) -> quinn::Endpoint {
    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![certified.cert_der.clone()],
            rustls::pki_types::PrivateKeyDer::try_from(certified.key_der.clone()).unwrap(),
        )
        .unwrap();
    crypto.alpn_protocols = vec![b"h3".to_vec()];
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(crypto).unwrap(),
    ));
    let endpoint = quinn::Endpoint::server(
        server_config,
        format!("[::1]:{port}").parse::<SocketAddr>().unwrap(),
    )
    .unwrap();
    let accepting = endpoint.clone();
    tokio::spawn(async move {
        while let Some(incoming) = accepting.accept().await {
            let _ = incoming.await;
        }
    });
    endpoint
}

async fn delayed_relay(front_port: u16, target_port: u16) -> tokio::task::JoinHandle<()> {
    let front = Arc::new(
        UdpSocket::bind(("127.0.0.1", front_port))
            .await
            .expect("bind relay on 127.0.0.1 at the port of the [::1] server"),
    );
    let back = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    back.connect(("127.0.0.1", target_port)).await.unwrap();
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
                    tokio::time::sleep(RELAY_DELAY).await;
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
                tokio::time::sleep(RELAY_DELAY).await;
                let _ = back.send(&datagram).await;
            });
        }
        replies.abort();
    })
}

async fn connect(url: &str, ca: Vec<u8>, id: &str) -> mqtt5::error::Result<()> {
    let client = MqttClient::new(id);
    client.set_tls_config(None, None, Some(ca)).await.unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        client.connect_with_options(url, ConnectOptions::new(id).with_automatic_reconnect(false)),
    )
    .await
    .expect("connect did not finish in 20 s")
    .map(|_| ());
    let _ = client.disconnect().await;
    outcome
}

#[tokio::test]
async fn control_the_relayed_broker_accepts_on_its_ipv4_address() {
    let good = certified();
    let (good_port, good_running) = quic_broker(&good, "127.0.0.1:0").await;
    let wrong = certified();
    let (port, wrong_running) = quic_broker(&wrong, "[::1]:0").await;
    let relay = delayed_relay(port, good_port).await;
    let outcome = connect(
        &format!("quics://127.0.0.1:{port}"),
        good.cert_pem.clone(),
        "review373-control",
    )
    .await;
    relay.abort();
    good_running.abort();
    wrong_running.abort();
    assert!(outcome.is_ok(), "{outcome:?}");
}

#[tokio::test]
async fn a_different_server_on_ipv6_with_an_untrusted_certificate_does_not_end_the_connect() {
    let good = certified();
    let (good_port, good_running) = quic_broker(&good, "127.0.0.1:0").await;
    let wrong = certified();
    let (port, wrong_running) = quic_broker(&wrong, "[::1]:0").await;
    let relay = delayed_relay(port, good_port).await;
    let outcome = connect(
        &format!("quics://localhost:{port}"),
        good.cert_pem.clone(),
        "review373-wrong-ca",
    )
    .await;
    relay.abort();
    good_running.abort();
    wrong_running.abort();
    assert!(outcome.is_ok(), "{outcome:?}");
}

#[tokio::test]
async fn a_server_on_ipv6_speaking_another_protocol_does_not_end_the_connect() {
    let good = certified();
    let (good_port, good_running) = quic_broker(&good, "127.0.0.1:0").await;
    let other = other_protocol_server(&good, 0);
    let port = other.local_addr().unwrap().port();
    let relay = delayed_relay(port, good_port).await;
    let outcome = connect(
        &format!("quics://localhost:{port}"),
        good.cert_pem.clone(),
        "review373-alpn",
    )
    .await;
    relay.abort();
    good_running.abort();
    other.close(0u32.into(), b"");
    assert!(outcome.is_ok(), "{outcome:?}");
}
