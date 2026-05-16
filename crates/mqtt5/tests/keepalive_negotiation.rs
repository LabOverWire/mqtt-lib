mod common;

use common::TestBroker;

use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, ConnectionEvent, MqttClient};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::test]
async fn server_keep_alive_overrides_client_request() {
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };
    let mut config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config);
    config.server_keep_alive = Some(Duration::from_secs(10));

    let broker = TestBroker::start_with_config(config).await;

    let mut options = ConnectOptions::new("kanego-1");
    options.keep_alive = Duration::from_secs(60);

    let observed = Arc::new(Mutex::new(None::<Duration>));
    let observed_clone = Arc::clone(&observed);

    let client = MqttClient::with_options(options);
    client
        .on_connection_event(move |event| {
            if let ConnectionEvent::Connected { keep_alive, .. } = event {
                let observed = Arc::clone(&observed_clone);
                tokio::spawn(async move {
                    *observed.lock().await = Some(keep_alive);
                });
            }
        })
        .await
        .unwrap();

    client.connect(broker.address()).await.unwrap();

    assert_eq!(client.keep_alive().await, Duration::from_secs(10));

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        *observed.lock().await,
        Some(Duration::from_secs(10)),
        "ConnectionEvent::Connected must carry the broker-negotiated keep-alive",
    );

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn keep_alive_falls_back_to_client_value_without_server_override() {
    let broker = TestBroker::start().await;

    let mut options = ConnectOptions::new("kanego-2");
    options.keep_alive = Duration::from_secs(30);

    let client = MqttClient::with_options(options);
    client.connect(broker.address()).await.unwrap();

    assert_eq!(client.keep_alive().await, Duration::from_secs(30));

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn keep_alive_before_connect_returns_configured_value() {
    let mut options = ConnectOptions::new("kanego-3");
    options.keep_alive = Duration::from_secs(45);

    let client = MqttClient::with_options(options);
    assert_eq!(client.keep_alive().await, Duration::from_secs(45));
}

#[tokio::test]
async fn keep_alive_renegotiates_against_each_broker_on_reconnect() {
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };

    let mut config_a = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config.clone());
    config_a.server_keep_alive = Some(Duration::from_secs(10));
    let broker_a = TestBroker::start_with_config(config_a).await;

    let mut config_b = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config);
    config_b.server_keep_alive = Some(Duration::from_secs(25));
    let broker_b = TestBroker::start_with_config(config_b).await;

    let mut options = ConnectOptions::new("kanego-4");
    options.keep_alive = Duration::from_secs(60);

    let client = MqttClient::with_options(options);

    client.connect(broker_a.address()).await.unwrap();
    assert_eq!(
        client.keep_alive().await,
        Duration::from_secs(10),
        "first CONNECT must adopt broker A's ServerKeepAlive",
    );
    client.disconnect().await.unwrap();

    client.connect(broker_b.address()).await.unwrap();
    assert_eq!(
        client.keep_alive().await,
        Duration::from_secs(25),
        "reconnect must re-negotiate from the original 60s request, not the previously-negotiated 10s",
    );
    client.disconnect().await.unwrap();
}
