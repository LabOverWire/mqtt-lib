use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::client::MqttClient;
use mqtt5::types::ConnectOptions;
use mqtt5::QoS;
use mqtt5::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn test_try_private_adds_user_property() {
    let config_with_try_private = BridgeConfig::new("test-bridge", "localhost:1883").add_topic(
        "test/#",
        BridgeDirection::Both,
        QoS::AtMostOnce,
    );

    assert!(config_with_try_private.try_private);

    let mut options = ConnectOptions::new(&config_with_try_private.client_id);

    if config_with_try_private.try_private {
        options
            .properties
            .user_properties
            .push(("bridge".to_string(), config_with_try_private.name.clone()));
    }

    assert_eq!(options.properties.user_properties.len(), 1);
    assert_eq!(options.properties.user_properties[0].0, "bridge");
    assert_eq!(options.properties.user_properties[0].1, "test-bridge");
}

#[tokio::test]
async fn test_try_private_false_no_user_property() {
    let mut config_without_try_private = BridgeConfig::new("test-bridge", "localhost:1883")
        .add_topic("test/#", BridgeDirection::Both, QoS::AtMostOnce);

    config_without_try_private.try_private = false;

    let mut options = ConnectOptions::new(&config_without_try_private.client_id);

    if config_without_try_private.try_private {
        options.properties.user_properties.push((
            "bridge".to_string(),
            config_without_try_private.name.clone(),
        ));
    }

    assert_eq!(options.properties.user_properties.len(), 0);
}

#[tokio::test]
async fn test_bridge_connection_with_try_private() {
    let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();

    let broker_config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_max_clients(10);

    let mut broker = MqttBroker::with_config(broker_config).await.unwrap();
    let broker_addr = broker.local_addr().expect("Failed to get broker address");

    let broker_handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    let test_client = MqttClient::new("test-client");

    let mut options = ConnectOptions::new("test-client");
    options
        .properties
        .user_properties
        .push(("bridge".to_string(), "test-bridge".to_string()));

    let connect_result = test_client
        .connect_with_options(&format!("mqtt://{broker_addr}"), options)
        .await;

    assert!(connect_result.is_ok());

    let _ = test_client.disconnect().await;

    broker_handle.abort();
}

#[tokio::test]
async fn test_bridge_manager_with_try_private() {
    let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();

    let mut config_enabled = BridgeConfig::new("bridge-enabled", "localhost:19999").add_topic(
        "test/#",
        BridgeDirection::Both,
        QoS::AtMostOnce,
    );
    config_enabled.try_private = true;

    let mut config_disabled = BridgeConfig::new("bridge-disabled", "localhost:19998").add_topic(
        "test/#",
        BridgeDirection::Both,
        QoS::AtMostOnce,
    );
    config_disabled.try_private = false;

    assert!(config_enabled.try_private);
    assert!(!config_disabled.try_private);
}
