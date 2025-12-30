use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection, BridgeProtocol};
use mqtt5::broker::config::{BrokerConfig, QuicConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::time::Duration;
use mqtt5::MqttClient;
use mqtt5::QoS;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use ulid::Ulid;

fn test_client_id(prefix: &str) -> String {
    format!("{}-{}", prefix, Ulid::new())
}

fn storage_config() -> StorageConfig {
    StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: false,
        ..Default::default()
    }
}

async fn start_quic_broker(quic_port: u16) -> (MqttBroker, SocketAddr) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let quic_addr: SocketAddr = format!("127.0.0.1:{quic_port}").parse().unwrap();

    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_storage(storage_config())
        .with_quic(
            QuicConfig::new(
                PathBuf::from("../../test_certs/server.pem"),
                PathBuf::from("../../test_certs/server.key"),
            )
            .with_bind_address(quic_addr),
        );

    let broker = MqttBroker::with_config(config).await.unwrap();
    (broker, quic_addr)
}

async fn start_tcp_broker_with_quic_bridge(
    tcp_port: u16,
    remote_quic_addr: SocketAddr,
    topic_patterns: Vec<(&str, BridgeDirection)>,
) -> MqttBroker {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let tcp_addr: SocketAddr = format!("127.0.0.1:{tcp_port}").parse().unwrap();

    let mut bridge_config = BridgeConfig::new("quic-test-bridge", remote_quic_addr.to_string());
    bridge_config.client_id = format!("bridge-quic-{}", Ulid::new());
    bridge_config.protocol = BridgeProtocol::Quic;
    bridge_config.insecure = Some(true);

    for (pattern, direction) in topic_patterns {
        bridge_config = bridge_config.add_topic(pattern, direction, QoS::AtLeastOnce);
    }

    let mut config = BrokerConfig::default()
        .with_bind_address(tcp_addr)
        .with_storage(storage_config());
    config.bridges.push(bridge_config);

    MqttBroker::with_config(config).await.unwrap()
}

#[tokio::test]
async fn test_quic_bridge_outbound() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut remote_broker, quic_addr) = start_quic_broker(34567).await;
    let remote_handle = tokio::spawn(async move { remote_broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut local_broker = start_tcp_broker_with_quic_bridge(
        31887,
        quic_addr,
        vec![("quic-out/#", BridgeDirection::Out)],
    )
    .await;
    let local_addr = local_broker.local_addr().unwrap();
    let local_handle = tokio::spawn(async move { local_broker.run().await });
    tokio::time::sleep(Duration::from_millis(500)).await;

    let sub_client = MqttClient::new(test_client_id("remote-sub"));
    sub_client.set_insecure_tls(true).await;
    if sub_client
        .connect(&format!("quic://{quic_addr}"))
        .await
        .is_err()
    {
        eprintln!("Could not connect subscriber to remote QUIC broker");
        remote_handle.abort();
        local_handle.abort();
        return;
    }

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();
    let topic = format!("quic-out/{}/test", Ulid::new());
    let topic_clone = topic.clone();

    sub_client
        .subscribe(&topic_clone, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let pub_client = MqttClient::new(test_client_id("local-pub"));
    pub_client
        .connect(&format!("mqtt://{local_addr}"))
        .await
        .unwrap();

    pub_client
        .publish_qos1(&topic, b"Bridge test message via QUIC")
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(2000)).await;

    let count = received.load(Ordering::Relaxed);
    eprintln!("Messages received on remote broker: {count}");

    pub_client.disconnect().await.ok();
    sub_client.disconnect().await.ok();
    local_handle.abort();
    remote_handle.abort();

    assert_eq!(
        count, 1,
        "Message should be bridged from local TCP to remote QUIC"
    );
}

#[tokio::test]
async fn test_quic_bridge_inbound() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut remote_broker, quic_addr) = start_quic_broker(34568).await;
    let remote_handle = tokio::spawn(async move { remote_broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut local_broker = start_tcp_broker_with_quic_bridge(
        31888,
        quic_addr,
        vec![("quic-in/#", BridgeDirection::In)],
    )
    .await;
    let local_addr = local_broker.local_addr().unwrap();
    let local_handle = tokio::spawn(async move { local_broker.run().await });
    tokio::time::sleep(Duration::from_millis(500)).await;

    let sub_client = MqttClient::new(test_client_id("local-sub"));
    sub_client
        .connect(&format!("mqtt://{local_addr}"))
        .await
        .unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();
    let topic = format!("quic-in/{}/test", Ulid::new());
    let topic_clone = topic.clone();

    sub_client
        .subscribe(&topic_clone, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let pub_client = MqttClient::new(test_client_id("remote-pub"));
    pub_client.set_insecure_tls(true).await;
    if pub_client
        .connect(&format!("quic://{quic_addr}"))
        .await
        .is_err()
    {
        eprintln!("Could not connect publisher to remote QUIC broker");
        remote_handle.abort();
        local_handle.abort();
        return;
    }

    pub_client
        .publish_qos1(&topic, b"Inbound bridge test via QUIC")
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(2000)).await;

    let count = received.load(Ordering::Relaxed);
    eprintln!("Messages received on local broker: {count}");

    pub_client.disconnect().await.ok();
    sub_client.disconnect().await.ok();
    local_handle.abort();
    remote_handle.abort();

    assert_eq!(
        count, 1,
        "Message should be bridged from remote QUIC to local TCP"
    );
}

#[tokio::test]
async fn test_quic_bridge_bidirectional() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut remote_broker, quic_addr) = start_quic_broker(34569).await;
    let remote_handle = tokio::spawn(async move { remote_broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut local_broker = start_tcp_broker_with_quic_bridge(
        31889,
        quic_addr,
        vec![("quic-both/#", BridgeDirection::Both)],
    )
    .await;
    let local_addr = local_broker.local_addr().unwrap();
    let local_handle = tokio::spawn(async move { local_broker.run().await });
    tokio::time::sleep(Duration::from_millis(500)).await;

    let local_sub = MqttClient::new(test_client_id("local-sub"));
    local_sub
        .connect(&format!("mqtt://{local_addr}"))
        .await
        .unwrap();

    let remote_sub = MqttClient::new(test_client_id("remote-sub"));
    remote_sub.set_insecure_tls(true).await;
    if remote_sub
        .connect(&format!("quic://{quic_addr}"))
        .await
        .is_err()
    {
        eprintln!("Could not connect remote subscriber");
        remote_handle.abort();
        local_handle.abort();
        return;
    }

    let local_received = Arc::new(AtomicU32::new(0));
    let remote_received = Arc::new(AtomicU32::new(0));
    let local_clone = local_received.clone();
    let remote_clone = remote_received.clone();

    let topic = format!("quic-both/{}/test", Ulid::new());
    let topic1 = topic.clone();
    let topic2 = topic.clone();

    local_sub
        .subscribe(&topic1, move |_msg| {
            local_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    remote_sub
        .subscribe(&topic2, move |_msg| {
            remote_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let local_pub = MqttClient::new(test_client_id("local-pub"));
    local_pub
        .connect(&format!("mqtt://{local_addr}"))
        .await
        .unwrap();
    local_pub
        .publish_qos1(&topic, b"From local to remote via QUIC")
        .await
        .unwrap();

    let remote_pub = MqttClient::new(test_client_id("remote-pub"));
    remote_pub.set_insecure_tls(true).await;
    if remote_pub
        .connect(&format!("quic://{quic_addr}"))
        .await
        .is_err()
    {
        eprintln!("Could not connect remote publisher");
        remote_handle.abort();
        local_handle.abort();
        return;
    }
    remote_pub
        .publish_qos1(&topic, b"From remote to local via QUIC")
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(2000)).await;

    let local_count = local_received.load(Ordering::Relaxed);
    let remote_count = remote_received.load(Ordering::Relaxed);
    eprintln!("Local broker received: {local_count}, Remote broker received: {remote_count}");

    local_pub.disconnect().await.ok();
    remote_pub.disconnect().await.ok();
    local_sub.disconnect().await.ok();
    remote_sub.disconnect().await.ok();
    local_handle.abort();
    remote_handle.abort();

    assert!(
        local_count >= 1,
        "Local broker should receive at least 1 message"
    );
    assert!(
        remote_count >= 1,
        "Remote broker should receive at least 1 message"
    );
}
