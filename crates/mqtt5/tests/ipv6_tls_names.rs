#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::config::{LoadBalancerConfig, TlsConfig as BrokerTlsConfig};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, MqttBroker, StorageConfig};
use mqtt5::client::MqttClient;
use mqtt5::time::Duration;
use mqtt5::transport::tls::TlsConfig;
use mqtt5::{ConnectOptions, QoS};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

fn certs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test_certs")
}

fn options(id: &str) -> ConnectOptions {
    ConnectOptions::new(id).with_automatic_reconnect(false)
}

async fn tls_broker(bind: &str, redirect_to: Option<String>) -> SocketAddr {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls = std::net::TcpListener::bind(bind)
        .unwrap()
        .local_addr()
        .unwrap();
    let mut config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_storage(StorageConfig::new().with_persistence(false))
        .with_tls(
            BrokerTlsConfig::new(certs().join("server.pem"), certs().join("server.key"))
                .with_bind_address(tls),
        );
    if let Some(reference) = redirect_to {
        config = config.with_load_balancer(LoadBalancerConfig::new(vec![reference]));
    }
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let mut ready = broker.ready_receiver();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    tls
}

#[tokio::test]
async fn mqtts_to_an_ipv6_address_verifies_the_certificate_against_it() {
    let tls = tls_broker("[::1]:0", None).await;
    let client = MqttClient::new("ipv6-mqtts");
    client
        .set_tls_config(
            None,
            None,
            Some(std::fs::read(certs().join("ca.pem")).unwrap()),
        )
        .await;
    client
        .connect_with_options(
            &format!("mqtts://[::1]:{}", tls.port()),
            options("ipv6-mqtts"),
        )
        .await
        .unwrap();
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn connect_with_tls_follows_a_redirect_to_an_ipv6_address() {
    let backend = tls_broker("[::1]:0", None).await;
    let redirector = tls_broker("127.0.0.1:0", Some(format!("[::1]:{}", backend.port()))).await;
    let client = MqttClient::new("ipv6-redirect");
    let mut tls = TlsConfig::new(redirector, "127.0.0.1");
    tls.load_ca_cert_pem(certs().join("ca.pem").to_str().unwrap())
        .unwrap();
    client
        .connect_with_tls_and_options(tls, options("ipv6-redirect"))
        .await
        .unwrap();
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn a_tls_bridge_to_an_ipv6_address_verifies_the_certificate_against_it() {
    let tls = tls_broker("[::1]:0", None).await;
    let mut config = BridgeConfig::new("ipv6-bridge", format!("[::1]:{}", tls.port())).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.use_tls = true;
    config.ca_file = Some(certs().join("ca.pem").to_string_lossy().into_owned());
    config.retry_jitter = false;
    config.max_reconnect_attempts = Some(1);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    let connected = tokio::time::timeout(Duration::from_secs(4), async {
        while !bridge.get_stats().await.connected {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let last_error = bridge.get_stats().await.last_error;
    let _ = bridge.stop().await;
    task.abort();
    assert!(connected.is_ok(), "{last_error:?}");
}
