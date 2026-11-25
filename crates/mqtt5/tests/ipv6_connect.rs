#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::config::TlsConfig as BrokerTlsConfig;
use mqtt5::broker::config::{BrokerConfig, QuicConfig, StorageBackend, StorageConfig};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient, QoS};
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const LINK_LOCAL: Ipv6Addr = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1);

enum Listener {
    Tls,
    Quic,
}

struct Certificate {
    dir: tempfile::TempDir,
    pem: Vec<u8>,
}

impl Certificate {
    fn for_names(names: &[&str]) -> Self {
        let certified = rcgen::generate_simple_self_signed(
            names.iter().map(ToString::to_string).collect::<Vec<_>>(),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pem = certified.cert.pem().into_bytes();
        std::fs::write(dir.path().join("cert.pem"), &pem).unwrap();
        std::fs::write(
            dir.path().join("key.pem"),
            certified.signing_key.serialize_pem(),
        )
        .unwrap();
        Self { dir, pem }
    }

    fn cert(&self) -> std::path::PathBuf {
        self.dir.path().join("cert.pem")
    }

    fn key(&self) -> std::path::PathBuf {
        self.dir.path().join("key.pem")
    }
}

fn link_local_scope() -> Option<u32> {
    (1..=32).find(|&scope| {
        std::net::TcpListener::bind(SocketAddrV6::new(LINK_LOCAL, 0, 0, scope)).is_ok()
    })
}

async fn broker_on(
    listener: &Listener,
    bind: SocketAddr,
    cert: &Path,
    key: &Path,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = BrokerConfig::default()
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap());
    let config = match listener {
        Listener::Tls => {
            config.with_tls(BrokerTlsConfig::new(cert.into(), key.into()).with_bind_address(bind))
        }
        Listener::Quic => {
            config.with_quic(QuicConfig::new(cert.into(), key.into()).with_bind_address(bind))
        }
    };
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let address = match listener {
        Listener::Tls => broker.tls_local_addr(),
        Listener::Quic => broker.quic_local_addr(),
    }
    .unwrap();
    let mut ready = broker.ready_receiver();
    let running = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    (address, running)
}

async fn connect(
    listener: Listener,
    bind: SocketAddr,
    certificate: &Certificate,
    url: impl FnOnce(u16) -> String,
    insecure: bool,
) -> mqtt5::Result<()> {
    let (address, running) =
        broker_on(&listener, bind, &certificate.cert(), &certificate.key()).await;
    let client = MqttClient::new("ipv6-connect");
    if insecure {
        client.set_insecure_tls(true).await;
    } else {
        client
            .set_tls_config(None, None, Some(certificate.pem.clone()))
            .await;
    }
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        client.connect_with_options(
            &url(address.port()),
            ConnectOptions::new("ipv6-connect").with_automatic_reconnect(false),
        ),
    )
    .await
    .unwrap_or(Err(mqtt5::MqttError::Timeout))
    .map(|_| ());
    let _ = client.disconnect().await;
    running.abort();
    result
}

fn loopback_v6() -> SocketAddr {
    SocketAddr::from((Ipv6Addr::LOCALHOST, 0))
}

#[tokio::test]
async fn quic_connects_to_an_ipv6_address() {
    let certificate = Certificate::for_names(&["localhost"]);
    connect(
        Listener::Quic,
        loopback_v6(),
        &certificate,
        |port| format!("quic://[::1]:{port}"),
        true,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn quics_to_an_ipv6_address_verifies_the_certificate_against_it() {
    let certificate = Certificate::for_names(&["::1"]);
    connect(
        Listener::Quic,
        loopback_v6(),
        &certificate,
        |port| format!("quics://[::1]:{port}"),
        false,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn quics_to_an_ipv6_address_refuses_a_certificate_for_another_name() {
    let certificate = Certificate::for_names(&["localhost"]);
    let error = connect(
        Listener::Quic,
        loopback_v6(),
        &certificate,
        |port| format!("quics://[::1]:{port}"),
        false,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("certificate not valid for name \"::1\""),
        "{error:?}"
    );
}

#[tokio::test]
async fn mqtts_to_a_link_local_address_with_a_numeric_zone_verifies_the_address() {
    let Some(scope) = link_local_scope() else {
        return;
    };
    let certificate = Certificate::for_names(&["fe80::1"]);
    connect(
        Listener::Tls,
        SocketAddrV6::new(LINK_LOCAL, 0, 0, scope).into(),
        &certificate,
        |port| format!("mqtts://[fe80::1%{scope}]:{port}"),
        false,
    )
    .await
    .unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn mqtts_to_a_link_local_address_with_a_named_zone_verifies_the_address() {
    let Some(scope) = link_local_scope() else {
        return;
    };
    let certificate = Certificate::for_names(&["fe80::1"]);
    connect(
        Listener::Tls,
        SocketAddrV6::new(LINK_LOCAL, 0, 0, scope).into(),
        &certificate,
        |port| format!("mqtts://[fe80::1%lo0]:{port}"),
        false,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn mqtts_to_a_link_local_address_refuses_a_certificate_for_another_address() {
    let Some(scope) = link_local_scope() else {
        return;
    };
    let certificate = Certificate::for_names(&["::1"]);
    let error = connect(
        Listener::Tls,
        SocketAddrV6::new(LINK_LOCAL, 0, 0, scope).into(),
        &certificate,
        |port| format!("mqtts://[fe80::1%{scope}]:{port}"),
        false,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("certificate not valid for name \"fe80::1\""),
        "{error:?}"
    );
}

#[tokio::test]
async fn quics_to_a_link_local_address_with_a_zone_verifies_the_address() {
    let Some(scope) = link_local_scope() else {
        return;
    };
    let certificate = Certificate::for_names(&["fe80::1"]);
    connect(
        Listener::Quic,
        SocketAddrV6::new(LINK_LOCAL, 0, 0, scope).into(),
        &certificate,
        |port| format!("quics://[fe80::1%{scope}]:{port}"),
        false,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_tls_bridge_to_a_link_local_address_with_a_zone_verifies_the_address() {
    let Some(scope) = link_local_scope() else {
        return;
    };
    let certificate = Certificate::for_names(&["fe80::1"]);
    let (address, running) = broker_on(
        &Listener::Tls,
        SocketAddrV6::new(LINK_LOCAL, 0, 0, scope).into(),
        &certificate.cert(),
        &certificate.key(),
    )
    .await;
    let mut config = BridgeConfig::new(
        "link-local-bridge",
        format!("[fe80::1%{scope}]:{}", address.port()),
    )
    .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce);
    config.use_tls = true;
    config.ca_file = Some(certificate.cert().to_string_lossy().into_owned());
    config.retry_jitter = false;
    config.max_reconnect_attempts = Some(1);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let bridged = Arc::clone(&bridge);
    let task = tokio::spawn(async move { bridged.run().await });
    let connected = tokio::time::timeout(Duration::from_secs(4), async {
        while !bridge.get_stats().await.connected {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let last_error = bridge.get_stats().await.last_error;
    let _ = bridge.stop().await;
    task.abort();
    running.abort();
    assert!(connected.is_ok(), "{last_error:?}");
}

#[tokio::test]
async fn mqtts_to_a_link_local_address_sends_no_server_name() {
    use tokio::io::AsyncReadExt;
    let Some(scope) = link_local_scope() else {
        return;
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = tokio::net::TcpListener::bind(SocketAddrV6::new(LINK_LOCAL, 0, 0, scope))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let hello = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut hello = vec![0u8; 4096];
        let read = stream.read(&mut hello).await.unwrap();
        hello.truncate(read);
        hello
    });
    let client = MqttClient::new("ipv6-connect");
    client
        .set_tls_config(None, None, Some(Certificate::for_names(&["fe80::1"]).pem))
        .await;
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        client.connect_with_options(
            &format!("mqtts://[fe80::1%{scope}]:{port}"),
            ConnectOptions::new("ipv6-connect").with_automatic_reconnect(false),
        ),
    )
    .await;
    let hello = tokio::time::timeout(Duration::from_secs(2), hello)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hello.first(), Some(&0x16), "not a TLS handshake: {hello:?}");
    assert!(
        !hello.windows(4).any(|window| window == b"fe80"),
        "the address was sent as SNI"
    );
}
