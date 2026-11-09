#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection, BridgeProtocol};
use mqtt5::broker::config::{BrokerConfig, QuicConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient, QoS};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

async fn quics_connect_to_ip_with_certificate_for(names: &[&str]) -> mqtt5::error::Result<()> {
    quics_connect_to_ip(names, None).await
}

async fn quics_connect_to_ip(
    names: &[&str],
    pinned: Option<(&str, &str)>,
) -> mqtt5::error::Result<()> {
    quics_connect("127.0.0.1", names, pinned).await
}

async fn quics_connect(
    host: &str,
    names: &[&str],
    pinned: Option<(&str, &str)>,
) -> mqtt5::error::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certified = rcgen::generate_simple_self_signed(
        names.iter().map(ToString::to_string).collect::<Vec<_>>(),
    )
    .unwrap();
    let cert = certified.cert.pem().into_bytes();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cert.pem"), &cert).unwrap();
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
    let address = broker.quic_local_addr().unwrap();
    let running = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    let client = MqttClient::new("quic-server-name");
    client.set_tls_config(None, None, Some(cert)).await;
    if let Some((host, server_name)) = pinned {
        client.set_quic_server_name(host, server_name).await;
    }
    let result = client
        .connect_with_options(
            &format!("quics://{host}:{}", address.port()),
            ConnectOptions::new("quic-server-name").with_automatic_reconnect(false),
        )
        .await
        .map(|_| ());
    running.abort();
    result
}

#[tokio::test]
async fn a_certificate_for_localhost_alone_is_refused_for_an_ip_address() {
    assert!(quics_connect_to_ip_with_certificate_for(&["localhost"])
        .await
        .is_err());
}

#[tokio::test]
async fn a_certificate_naming_the_ip_address_is_accepted() {
    quics_connect_to_ip_with_certificate_for(&["127.0.0.1"])
        .await
        .unwrap();
}

async fn bridge_over_quic_delivers(server_name: Option<&str>) -> bool {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    std::fs::write(&cert, certified.cert.pem()).unwrap();
    std::fs::write(
        dir.path().join("key.pem"),
        certified.signing_key.serialize_pem(),
    )
    .unwrap();
    let storage = || StorageConfig::default().with_backend(StorageBackend::Memory);
    let any_port = || "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let mut remote = MqttBroker::with_config(
        BrokerConfig::default()
            .with_storage(storage())
            .with_bind_address(any_port())
            .with_quic(
                QuicConfig::new(cert.clone(), dir.path().join("key.pem"))
                    .with_bind_address(any_port()),
            ),
    )
    .await
    .unwrap();
    let remote_address = remote.quic_local_addr().unwrap();
    let remote_plain = remote.local_addr().unwrap();
    let remote_running = tokio::spawn(async move {
        let _ = remote.run().await;
    });

    let mut bridge = BridgeConfig::new("to-remote", remote_address.to_string());
    bridge.client_id = "to-remote-client".to_string();
    bridge.protocol = BridgeProtocol::QuicSecure;
    bridge.tls_server_name = server_name.map(ToString::to_string);
    bridge.ca_file = Some(cert.to_string_lossy().to_string());
    bridge.connection_retries = 1;
    bridge = bridge.add_topic("bridged/#", BridgeDirection::Out, QoS::AtLeastOnce);
    let mut local_config = BrokerConfig::default()
        .with_storage(storage())
        .with_bind_address(any_port());
    local_config.bridges.push(bridge);
    let mut local = MqttBroker::with_config(local_config).await.unwrap();
    let local_plain = local.local_addr().unwrap();
    let local_running = tokio::spawn(async move {
        let _ = local.run().await;
    });
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let delivered = Arc::new(AtomicBool::new(false));
    let seen = delivered.clone();
    let subscriber = MqttClient::new("remote-subscriber");
    subscriber
        .connect(&format!("mqtt://{remote_plain}"))
        .await
        .unwrap();
    subscriber
        .subscribe("bridged/#", move |_| seen.store(true, Ordering::SeqCst))
        .await
        .unwrap();
    let publisher = MqttClient::new("local-publisher");
    publisher
        .connect(&format!("mqtt://{local_plain}"))
        .await
        .unwrap();
    for _ in 0..4 {
        let _ = publisher.publish_qos1("bridged/x", b"x".to_vec()).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    remote_running.abort();
    local_running.abort();
    delivered.load(Ordering::SeqCst)
}

#[tokio::test]
async fn a_quic_bridge_to_an_ip_address_verifies_against_its_server_name() {
    assert!(bridge_over_quic_delivers(Some("localhost")).await);
}

#[tokio::test]
async fn a_quic_bridge_to_an_ip_address_without_a_server_name_verifies_the_address() {
    assert!(!bridge_over_quic_delivers(None).await);
}

#[tokio::test]
async fn a_server_name_applies_to_the_host_it_was_set_for() {
    quics_connect_to_ip(&["localhost"], Some(("127.0.0.1", "localhost")))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_server_name_does_not_apply_to_another_host() {
    assert!(
        quics_connect_to_ip(&["localhost"], Some(("192.0.2.1", "localhost")))
            .await
            .is_err()
    );
    quics_connect_to_ip(&["127.0.0.1"], Some(("192.0.2.1", "localhost")))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_server_name_applies_to_its_host_in_any_case() {
    quics_connect(
        "localhost",
        &["broker.example"],
        Some(("LOCALHOST", "broker.example")),
    )
    .await
    .unwrap();
}
