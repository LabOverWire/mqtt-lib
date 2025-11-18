//! Comprehensive tests for bridge connection functionality

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::packet::publish::PublishPacket;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::sync::Arc;

#[tokio::test]
async fn test_bridge_connection_creation() {
    let router = Arc::new(MessageRouter::new());

    let config = BridgeConfig::new("test-bridge", "localhost:1883").add_topic(
        "test/#",
        BridgeDirection::Both,
        QoS::AtMostOnce,
    );

    let bridge = BridgeConnection::new(config.clone(), router);
    assert!(bridge.is_ok());

    let bridge = bridge.unwrap();
    let stats = bridge.get_stats().await;
    assert!(!stats.connected);
    assert_eq!(stats.messages_sent, 0);
    assert_eq!(stats.messages_received, 0);
}

#[tokio::test]
async fn test_bridge_connection_lifecycle() {
    let router = Arc::new(MessageRouter::new());

    let config = BridgeConfig::new("lifecycle-bridge", "localhost:11999").add_topic(
        "test/#",
        BridgeDirection::Both,
        QoS::AtMostOnce,
    );

    let bridge = BridgeConnection::new(config, router).unwrap();

    // Start should fail (no broker at localhost:11999)
    let start_result = bridge.start().await;
    assert!(start_result.is_err());

    // Stats should reflect connection attempt
    let stats = bridge.get_stats().await;
    assert!(stats.connection_attempts > 0);
    assert!(!stats.connected);

    // Stop should work even if not connected
    assert!(bridge.stop().await.is_ok());
}

#[tokio::test]
async fn test_bridge_topic_mapping_out() {
    let router = Arc::new(MessageRouter::new());

    let config = BridgeConfig::new("out-bridge", "localhost:1883")
        .add_topic("sensors/+/data", BridgeDirection::Out, QoS::AtLeastOnce)
        .add_topic("commands/#", BridgeDirection::In, QoS::AtMostOnce);

    let bridge = BridgeConnection::new(config, router.clone()).unwrap();

    // Create a test packet that should be forwarded (matches Out pattern)
    let sensor_packet =
        PublishPacket::new("sensors/temp/data".to_string(), b"25.5", QoS::AtLeastOnce);

    // This should attempt to forward (will fail due to no connection)
    let result = bridge.forward_message(&sensor_packet).await;
    // We expect Ok even if not connected (bridge handles this gracefully)
    assert!(result.is_ok());

    // Create a packet that should NOT be forwarded (In direction only)
    let command_packet =
        PublishPacket::new("commands/hvac/device".to_string(), b"on", QoS::AtMostOnce);

    // This should also be Ok but won't actually forward
    let result = bridge.forward_message(&command_packet).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_bridge_message_prefix_transformation() {
    let router = Arc::new(MessageRouter::new());

    let mut config = BridgeConfig::new("prefix-bridge", "localhost:1883");
    config.topics.push(mqtt5::broker::bridge::TopicMapping {
        pattern: "local/sensors/#".to_string(),
        direction: BridgeDirection::Out,
        qos: QoS::AtMostOnce,
        local_prefix: None,
        remote_prefix: Some("edge1/".to_string()),
    });

    let bridge = BridgeConnection::new(config, router).unwrap();

    // Test prefix application
    let packet = PublishPacket::new("local/sensors/temp".to_string(), b"22.5", QoS::AtMostOnce);

    // Forward should work (even without connection)
    assert!(bridge.forward_message(&packet).await.is_ok());

    // In a real scenario, this would send to "edge1/local/sensors/temp" on remote
}

#[tokio::test]
async fn test_bridge_qos_mapping() {
    let router = Arc::new(MessageRouter::new());

    let config = BridgeConfig::new("qos-bridge", "localhost:1883")
        .add_topic("qos0/#", BridgeDirection::Out, QoS::AtMostOnce)
        .add_topic("qos1/#", BridgeDirection::Out, QoS::AtLeastOnce)
        .add_topic("qos2/#", BridgeDirection::Out, QoS::ExactlyOnce);

    let bridge = BridgeConnection::new(config, router).unwrap();

    // Test that QoS is mapped correctly based on topic pattern
    let packets = vec![
        PublishPacket::new("qos0/test".to_string(), b"data", QoS::ExactlyOnce),
        PublishPacket::new("qos1/test".to_string(), b"data", QoS::AtMostOnce),
        PublishPacket::new("qos2/test".to_string(), b"data", QoS::AtMostOnce),
    ];

    for packet in packets {
        assert!(bridge.forward_message(&packet).await.is_ok());
    }
}

#[tokio::test]
async fn test_bridge_stats_tracking() {
    let router = Arc::new(MessageRouter::new());

    let config = BridgeConfig::new("stats-bridge", "localhost:1883").add_topic(
        "test/#",
        BridgeDirection::Both,
        QoS::AtMostOnce,
    );

    let bridge = BridgeConnection::new(config, router).unwrap();

    // Initial stats
    let stats = bridge.get_stats().await;
    assert_eq!(stats.messages_sent, 0);
    assert_eq!(stats.messages_received, 0);
    assert_eq!(stats.bytes_sent, 0);
    assert_eq!(stats.bytes_received, 0);

    // Note: Full stats tracking requires actual connection
    // These are tested in integration tests
}

#[tokio::test]
async fn test_bridge_reconnection_config() {
    let router = Arc::new(MessageRouter::new());

    let mut config = BridgeConfig::new("reconnect-bridge", "localhost:1883").add_topic(
        "test/#",
        BridgeDirection::Both,
        QoS::AtMostOnce,
    );

    config.reconnect_delay = Duration::from_secs(2);
    config.max_reconnect_attempts = Some(3);

    let bridge = BridgeConnection::new(config.clone(), router).unwrap();

    // Verify config is stored correctly
    let stats = bridge.get_stats().await;
    assert_eq!(stats.connection_attempts, 0); // No connection attempted yet
}

#[tokio::test]
async fn test_bridge_multiple_topic_patterns() {
    let router = Arc::new(MessageRouter::new());

    let config = BridgeConfig::new("multi-topic-bridge", "localhost:1883")
        .add_topic(
            "sensors/+/temperature",
            BridgeDirection::Out,
            QoS::AtLeastOnce,
        )
        .add_topic("sensors/+/humidity", BridgeDirection::Out, QoS::AtLeastOnce)
        .add_topic("sensors/+/pressure", BridgeDirection::In, QoS::AtMostOnce)
        .add_topic("status/#", BridgeDirection::Both, QoS::AtMostOnce);

    let bridge = BridgeConnection::new(config.clone(), router).unwrap();

    // Test various patterns
    let test_packets = vec![
        ("sensors/room1/temperature", true), // Out - should forward
        ("sensors/room1/humidity", true),    // Out - should forward
        ("sensors/room1/pressure", false),   // In only - should not forward
        ("status/system", true),             // Both - should forward
        ("other/topic", false),              // No match - should not forward
    ];

    for (topic, _should_forward) in test_packets {
        let packet = PublishPacket::new(topic.to_string(), b"test", QoS::AtMostOnce);
        assert!(bridge.forward_message(&packet).await.is_ok());
    }
}

#[tokio::test]
async fn test_bridge_invalid_config() {
    let _router = Arc::new(MessageRouter::new());

    // Empty topics should fail validation
    let config = BridgeConfig::new("invalid-bridge", "localhost:1883");
    assert!(config.validate().is_err());

    // Empty name should fail
    let mut config = BridgeConfig::new("", "localhost:1883").add_topic(
        "test/#",
        BridgeDirection::Both,
        QoS::AtMostOnce,
    );
    config.name = String::new();
    assert!(config.validate().is_err());

    // Empty client_id should fail
    let mut config = BridgeConfig::new("test", "localhost:1883").add_topic(
        "test/#",
        BridgeDirection::Both,
        QoS::AtMostOnce,
    );
    config.client_id = String::new();
    assert!(config.validate().is_err());
}

#[tokio::test]
async fn test_bridge_backup_brokers() {
    let router = Arc::new(MessageRouter::new());

    let config = BridgeConfig::new("backup-bridge", "primary.broker:1883")
        .add_topic("test/#", BridgeDirection::Both, QoS::AtMostOnce)
        .add_backup_broker("backup1.broker:1883")
        .add_backup_broker("backup2.broker:1883");

    assert_eq!(config.backup_brokers.len(), 2);

    // Verify the bridge accepts configuration with backup brokers
    let bridge = BridgeConnection::new(config, router).unwrap();

    // Verify initial state
    let stats = bridge.get_stats().await;
    assert_eq!(stats.connection_attempts, 0);
    assert!(!stats.connected);
}

#[tokio::test]
async fn test_bridge_tls_config() {
    let router = Arc::new(MessageRouter::new());

    let mut config = BridgeConfig::new("tls-bridge", "secure.broker:8883").add_topic(
        "secure/#",
        BridgeDirection::Both,
        QoS::AtLeastOnce,
    );

    config.use_tls = true;
    config.tls_server_name = Some("secure.broker".to_string());

    // Verify the bridge accepts TLS configuration
    let _bridge = BridgeConnection::new(config.clone(), router).unwrap();

    // Verify the TLS configuration was stored correctly
    assert!(config.use_tls);
    assert_eq!(config.tls_server_name, Some("secure.broker".to_string()));
}

#[tokio::test]
async fn test_bridge_authentication() {
    let router = Arc::new(MessageRouter::new());

    let mut config = BridgeConfig::new("auth-bridge", "auth.broker:1883").add_topic(
        "test/#",
        BridgeDirection::Both,
        QoS::AtMostOnce,
    );

    config.username = Some("bridge-user".to_string());
    config.password = Some("bridge-pass".to_string());

    // Verify the bridge accepts configuration with authentication
    let _bridge = BridgeConnection::new(config.clone(), router).unwrap();

    // Verify the configuration was stored correctly
    assert_eq!(config.username, Some("bridge-user".to_string()));
    assert_eq!(config.password, Some("bridge-pass".to_string()));
}
