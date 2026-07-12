#![cfg(feature = "broker")]
//! Real end-to-end validation of broker startup behaviour with bridge configs.
//!
//! Two live broker instances verify that a valid bridge still forwards messages,
//! and that every `BridgeConfig` validation failure point now aborts broker
//! startup instead of being logged and silently ignored.

use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
use mqtt5::broker::config::{StorageBackend, StorageConfig};
use mqtt5::broker::{BrokerConfig, HotReloadManager, MqttBroker};
use mqtt5::client::MqttClient;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::net::SocketAddr;
use tempfile::NamedTempFile;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

fn memory_storage() -> StorageConfig {
    StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    }
}

fn base_config() -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_max_clients(10)
        .with_storage(memory_storage())
}

async fn spawn_broker(config: BrokerConfig) -> (SocketAddr, JoinHandle<()>) {
    let mut broker = MqttBroker::with_config(config)
        .await
        .expect("broker with valid config must start");
    let addr = broker.local_addr().expect("broker must expose local addr");
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    (addr, handle)
}

#[tokio::test]
async fn valid_bridge_forwards_between_two_live_brokers() {
    let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();

    let (upstream_addr, upstream_handle) = spawn_broker(base_config()).await;

    let bridge = BridgeConfig::new("edge-to-upstream", upstream_addr.to_string()).add_topic(
        "sensors/#",
        BridgeDirection::Out,
        QoS::AtLeastOnce,
    );
    let mut edge_config = base_config();
    edge_config.bridges.push(bridge);

    let (edge_addr, edge_handle) = spawn_broker(edge_config).await;

    sleep(Duration::from_millis(300)).await;

    let subscriber = MqttClient::new("upstream-subscriber");
    subscriber
        .connect(&format!("mqtt://{upstream_addr}"))
        .await
        .unwrap();

    let (tx, mut rx) = mpsc::channel(4);
    subscriber
        .subscribe("sensors/+/data", move |msg| {
            let _ = tx.try_send((msg.topic.clone(), msg.payload.clone()));
        })
        .await
        .unwrap();

    let publisher = MqttClient::new("edge-publisher");
    publisher
        .connect(&format!("mqtt://{edge_addr}"))
        .await
        .unwrap();

    sleep(Duration::from_millis(300)).await;

    publisher
        .publish_qos1("sensors/temp/data", b"25.5C")
        .await
        .unwrap();

    let received = timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("bridged message must arrive within timeout")
        .expect("channel must yield the bridged message");

    assert_eq!(received.0, "sensors/temp/data");
    assert_eq!(received.1, b"25.5C");

    publisher.disconnect().await.unwrap();
    subscriber.disconnect().await.unwrap();
    edge_handle.abort();
    upstream_handle.abort();
}

async fn assert_startup_rejected(bridge: BridgeConfig, expected_fragment: &str) {
    let mut config = base_config();
    config.bridges.push(bridge);

    let Err(err) = MqttBroker::with_config(config).await else {
        panic!("broker startup must fail on invalid bridge config");
    };
    let message = err.to_string();
    assert!(
        message.contains(expected_fragment),
        "error '{message}' should mention '{expected_fragment}'"
    );
}

#[tokio::test]
async fn invalid_bridge_without_topics_rejects_startup() {
    let bridge = BridgeConfig::new("no-topics", "127.0.0.1:1883");
    assert_startup_rejected(bridge, "at least one topic mapping").await;
}

#[tokio::test]
async fn invalid_bridge_empty_name_rejects_startup() {
    let mut bridge = BridgeConfig::new("placeholder", "127.0.0.1:1883").add_topic(
        "sensors/#",
        BridgeDirection::Out,
        QoS::AtLeastOnce,
    );
    bridge.name = String::new();
    assert_startup_rejected(bridge, "name cannot be empty").await;
}

#[tokio::test]
async fn invalid_bridge_empty_client_id_rejects_startup() {
    let mut bridge = BridgeConfig::new("edge", "127.0.0.1:1883").add_topic(
        "sensors/#",
        BridgeDirection::Out,
        QoS::AtLeastOnce,
    );
    bridge.client_id = String::new();
    assert_startup_rejected(bridge, "Client ID cannot be empty").await;
}

#[tokio::test]
async fn invalid_bridge_empty_topic_pattern_rejects_startup() {
    let bridge = BridgeConfig::new("edge", "127.0.0.1:1883").add_topic(
        "",
        BridgeDirection::Out,
        QoS::AtLeastOnce,
    );
    assert_startup_rejected(bridge, "Topic pattern cannot be empty").await;
}

#[tokio::test]
async fn one_invalid_bridge_among_valid_ones_rejects_startup() {
    let valid = BridgeConfig::new("valid-edge", "127.0.0.1:1883").add_topic(
        "sensors/#",
        BridgeDirection::Out,
        QoS::AtLeastOnce,
    );
    let invalid = BridgeConfig::new("broken-edge", "127.0.0.1:1884");

    let mut config = base_config();
    config.bridges.push(valid);
    config.bridges.push(invalid);

    let result = MqttBroker::with_config(config).await;
    assert!(
        result.is_err(),
        "a single invalid bridge must abort startup even alongside valid bridges"
    );
}

#[tokio::test]
async fn broker_without_bridges_starts_normally() {
    let (_addr, handle) = spawn_broker(base_config()).await;
    handle.abort();
}

fn config_with_bridge(bridge: BridgeConfig) -> BrokerConfig {
    let mut config = base_config();
    config.bridges.push(bridge);
    config
}

async fn write_config(path: &std::path::Path, config: &BrokerConfig) {
    let json = serde_json::to_string_pretty(config).unwrap();
    tokio::fs::write(path, json).await.unwrap();
}

#[tokio::test]
async fn invalid_bridge_reload_is_rejected_and_previous_config_retained() {
    let valid_bridge = BridgeConfig::new("edge", "127.0.0.1:1883").add_topic(
        "sensors/#",
        BridgeDirection::Out,
        QoS::AtLeastOnce,
    );
    let initial = config_with_bridge(valid_bridge);

    let file = NamedTempFile::new().unwrap();
    write_config(file.path(), &initial).await;

    let manager = HotReloadManager::new(initial, file.path().to_path_buf()).unwrap();

    let invalid = config_with_bridge(BridgeConfig::new("edge", "127.0.0.1:1883"));
    write_config(file.path(), &invalid).await;

    let err = manager
        .reload_now()
        .await
        .expect_err("reload must reject an invalid bridge config");
    assert!(
        err.to_string().contains("at least one topic mapping"),
        "unexpected error: {err}"
    );

    let active = manager.get_config().await;
    assert_eq!(active.bridges.len(), 1);
    assert_eq!(active.bridges[0].topics.len(), 1);
}

#[tokio::test]
async fn valid_bridge_reload_is_applied() {
    let initial = config_with_bridge(BridgeConfig::new("edge", "127.0.0.1:1883").add_topic(
        "sensors/#",
        BridgeDirection::Out,
        QoS::AtLeastOnce,
    ));

    let file = NamedTempFile::new().unwrap();
    write_config(file.path(), &initial).await;

    let manager = HotReloadManager::new(initial, file.path().to_path_buf()).unwrap();

    let updated = config_with_bridge(
        BridgeConfig::new("edge", "127.0.0.1:1883")
            .add_topic("sensors/#", BridgeDirection::Out, QoS::AtLeastOnce)
            .add_topic("commands/#", BridgeDirection::In, QoS::AtMostOnce),
    );
    write_config(file.path(), &updated).await;

    let reloaded = manager
        .reload_now()
        .await
        .expect("valid bridge reload must succeed");
    assert!(reloaded, "reload should report a change was applied");

    let active = manager.get_config().await;
    assert_eq!(active.bridges[0].topics.len(), 2);
}
