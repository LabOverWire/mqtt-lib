#![cfg(all(feature = "broker", target_os = "macos"))]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::config::TlsConfig as BrokerTlsConfig;
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::MqttBroker;
use mqtt5::QoS;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::sync::Arc;
use std::time::Duration;

const LINK_LOCAL: Ipv6Addr = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1);

#[tokio::test]
async fn a_tls_bridge_to_a_link_local_address_with_a_named_zone_connects() {
    let Some(scope) = (1..=32).find(|&scope| {
        std::net::TcpListener::bind(SocketAddrV6::new(LINK_LOCAL, 0, 0, scope)).is_ok()
    }) else {
        return;
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certified = rcgen::generate_simple_self_signed(vec!["fe80::1".to_string()]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    std::fs::write(&cert, certified.cert.pem()).unwrap();
    std::fs::write(&key, certified.signing_key.serialize_pem()).unwrap();
    let config = BrokerConfig::default()
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_tls(
            BrokerTlsConfig::new(cert.clone(), key)
                .with_bind_address(SocketAddr::V6(SocketAddrV6::new(LINK_LOCAL, 0, 0, scope))),
        );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let port = broker.tls_local_addr().unwrap().port();
    let mut ready = broker.ready_receiver();
    let running = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    let mut config = BridgeConfig::new("named-zone-bridge", format!("[fe80::1%lo0]:{port}"))
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce);
    config.use_tls = true;
    config.ca_file = Some(cert.to_string_lossy().into_owned());
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
    assert!(connected.is_ok(), "scope {scope}: {last_error:?}");
}
