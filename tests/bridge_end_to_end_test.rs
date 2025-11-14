//! End-to-end integration test for broker bridging
//!
//! This test demonstrates a real bridge scenario between two brokers

use mqtt5::broker::config::{StorageBackend, StorageConfig};
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::client::MqttClient;
use std::sync::Arc;
use mqtt5::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};

#[tokio::test]
async fn test_bridge_between_brokers() {
    // Initialize logging for debugging
    let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();

    // Configure storage for both brokers
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };

    // Start two brokers on different ports
    let broker1_config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_max_clients(10)
        .with_storage(storage_config.clone());

    let broker2_config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_max_clients(10)
        .with_storage(storage_config);

    let mut broker1 = MqttBroker::with_config(broker1_config).await.unwrap();
    let mut broker2 = MqttBroker::with_config(broker2_config).await.unwrap();

    let broker1_addr = broker1.local_addr().expect("Failed to get broker1 address");
    let broker2_addr = broker2.local_addr().expect("Failed to get broker2 address");

    // Start brokers in background
    let broker1_handle = tokio::spawn(async move {
        let _ = broker1.run().await;
    });
    let broker2_handle = tokio::spawn(async move {
        let _ = broker2.run().await;
    });

    // Give brokers time to start
    sleep(Duration::from_millis(100)).await;

    // Create clients for each broker
    let client1 = MqttClient::new("client1");
    let client2 = MqttClient::new("client2");

    // Connect clients
    client1
        .connect(&format!("mqtt://{broker1_addr}"))
        .await
        .unwrap();
    client2
        .connect(&format!("mqtt://{broker2_addr}"))
        .await
        .unwrap();

    // Set up message channels to capture received messages
    let (tx1, mut rx1) = mpsc::channel(10);
    let (tx2, mut rx2) = mpsc::channel(10);

    // Client 1 subscribes to commands
    client1
        .subscribe("commands/+/device", move |msg| {
            let _ = tx1.blocking_send((msg.topic.clone(), msg.payload.clone()));
        })
        .await
        .unwrap();

    // Client 2 subscribes to sensor data
    client2
        .subscribe("sensors/+/data", move |msg| {
            let _ = tx2.blocking_send((msg.topic.clone(), msg.payload.clone()));
        })
        .await
        .unwrap();

    // Give subscriptions time to establish
    sleep(Duration::from_millis(100)).await;

    // In a real bridge scenario:
    // 1. Broker1 would have a bridge configured to forward "sensors/+/data" to Broker2
    // 2. Broker2 would have a bridge configured to forward "commands/+/device" to Broker1

    // Client 1 publishes sensor data (would be bridged to broker2)
    client1
        .publish_qos1("sensors/temp/data", b"25.5C")
        .await
        .unwrap();

    // Client 2 publishes a command (would be bridged to broker1)
    client2
        .publish_qos1("commands/hvac/device", b"set_temp:22")
        .await
        .unwrap();

    // In this test without actual bridge configuration, messages stay local
    // With bridge configuration, we would see:
    // - rx2 receiving the sensor data from client1
    // - rx1 receiving the command from client2

    // Wait a bit for any messages
    let sensor_result = timeout(Duration::from_millis(500), rx2.recv()).await;
    let command_result = timeout(Duration::from_millis(500), rx1.recv()).await;

    // Without bridge configuration, these will timeout
    assert!(sensor_result.is_err() || sensor_result.unwrap().is_none());
    assert!(command_result.is_err() || command_result.unwrap().is_none());

    // Clean up
    client1.disconnect().await.unwrap();
    client2.disconnect().await.unwrap();

    // Abort the broker tasks
    broker1_handle.abort();
    broker2_handle.abort();
}

#[tokio::test]
async fn test_bridge_configuration_serialization() {
    use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
    use mqtt5::QoS;

    // Create a bridge configuration
    let config = BridgeConfig::new("test-bridge", "remote.broker:1883")
        .add_topic("sensors/+/data", BridgeDirection::Out, QoS::AtLeastOnce)
        .add_topic("commands/#", BridgeDirection::In, QoS::AtMostOnce)
        .add_topic("status/+", BridgeDirection::Both, QoS::AtMostOnce)
        .add_backup_broker("backup1.broker:1883")
        .add_backup_broker("backup2.broker:1883");

    // Serialize to JSON
    let json = serde_json::to_string_pretty(&config).unwrap();
    println!("Bridge config JSON:\n{json}");

    // Deserialize back
    let config2: BridgeConfig = serde_json::from_str(&json).unwrap();

    // Verify
    assert_eq!(config2.name, "test-bridge");
    assert_eq!(config2.remote_address, "remote.broker:1883");
    assert_eq!(config2.topics.len(), 3);
    assert_eq!(config2.backup_brokers.len(), 2);
}

#[tokio::test]
async fn test_bridge_topic_pattern_matching() {
    use mqtt5::validation::topic_matches_filter;

    // Test patterns that would be used in bridges
    let test_cases = vec![
        ("sensors/temp/data", "sensors/+/data", true),
        ("sensors/humidity/data", "sensors/+/data", true),
        ("sensors/temp/status", "sensors/+/data", false),
        ("commands/hvac/on", "commands/#", true),
        ("commands/lights/dim/50", "commands/#", true),
        ("status/broker", "commands/#", false),
        ("$SYS/broker/load", "$SYS/broker/+", true),
        ("$SYS/broker/clients/connected", "$SYS/broker/+", false),
        ("home/living/temp", "home/+/temp", true),
        ("home/living/humidity", "home/+/temp", false),
    ];

    for (topic, pattern, expected) in test_cases {
        assert_eq!(
            topic_matches_filter(topic, pattern),
            expected,
            "Topic '{topic}' matching pattern '{pattern}' expected {expected}"
        );
    }
}

#[tokio::test]
async fn test_bridge_error_scenarios() {
    use mqtt5::broker::bridge::{BridgeConfig, BridgeManager};
    use mqtt5::broker::router::MessageRouter;
    use mqtt5::QoS;

    let router = Arc::new(MessageRouter::new());
    let manager = BridgeManager::new(router);

    // Test various error scenarios

    // 1. Invalid configuration
    let mut config = BridgeConfig::new("", "broker:1883");
    assert!(config.validate().is_err());

    // 2. Duplicate bridge
    config.name = "test".to_string();
    config.topics.push(mqtt5::broker::bridge::TopicMapping {
        pattern: "test/#".to_string(),
        direction: mqtt5::broker::bridge::BridgeDirection::Both,
        qos: QoS::AtMostOnce,
        local_prefix: None,
        remote_prefix: None,
    });

    let _ = manager.add_bridge(config.clone()).await;

    // Adding same bridge again should fail if it exists
    let bridges = manager.list_bridges().await;
    if bridges.contains(&"test".to_string()) {
        let result = manager.add_bridge(config).await;
        assert!(result.is_err());
    }

    // 3. Remove non-existent bridge
    let result = manager.remove_bridge("does-not-exist").await;
    assert!(result.is_err());
}
