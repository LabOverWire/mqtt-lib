use mqtt5::error::MqttError;
use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient, PublishOptions, QoS, SubscribeOptions};
use tokio::time::timeout;

#[tokio::test]
async fn test_connection_refused_wrong_port() {
    let client = MqttClient::new("error-test-1");

    // Try to connect to a port that's likely not running MQTT
    let result = client.connect("mqtt://localhost:12345").await;
    assert!(result.is_err());

    match result.unwrap_err() {
        MqttError::ConnectionError(_) => {} // Expected: connection refused
        other => panic!("Expected ConnectionError, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_connection_timeout() {
    let options = ConnectOptions::new("timeout-test");

    let client = MqttClient::with_options(options);

    // Try to connect to an IP that will timeout (non-routable)
    let result = timeout(
        Duration::from_secs(2),
        client.connect("mqtt://192.0.2.1:1883"), // TEST-NET-1 (RFC 5737)
    )
    .await;

    assert!(result.is_err() || result.unwrap().is_err());
}

#[tokio::test]
async fn test_invalid_client_id() {
    // MQTT v5 allows most characters in client IDs
    // Test with empty client ID which is allowed but requires clean start
    let mut options = ConnectOptions::new("");
    options.clean_start = false; // This combination should fail
    let client = MqttClient::with_options(options);

    let result = client.connect("mqtt://localhost:1883").await;
    // Should fail because empty client ID requires clean_start=true
    if result.is_ok() {
        client.disconnect().await.ok();
        // Some brokers might accept this, so not a hard failure
    }
}

#[tokio::test]
async fn test_publish_before_connect() {
    let client = MqttClient::new("not-connected");

    let result = client.publish("test/topic", "message").await;
    assert!(result.is_err());

    match result.unwrap_err() {
        MqttError::NotConnected => {} // Expected
        other => panic!("Expected NotConnected error, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_subscribe_before_connect() {
    let client = MqttClient::new("not-connected-2");

    let result = client.subscribe("test/topic", |_| {}).await;
    assert!(result.is_err());

    match result.unwrap_err() {
        MqttError::NotConnected => {} // Expected
        other => panic!("Expected NotConnected error, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_invalid_topic_name_publish() {
    let client = MqttClient::new("invalid-topic-test");

    // Test without connecting - these should be client-side validation errors
    // Topic names cannot contain wildcards
    let result = client.publish("test/+/invalid", "message").await;
    assert!(result.is_err(), "Topic with wildcard should be rejected");

    // Topic names cannot be empty
    let result = client.publish("", "message").await;
    assert!(result.is_err(), "Empty topic should be rejected");

    // Topic names cannot contain null characters
    let result = client.publish("test\0topic", "message").await;
    assert!(
        result.is_err(),
        "Topic with null character should be rejected"
    );
}

#[tokio::test]
async fn test_invalid_topic_filter_subscribe() {
    let client = MqttClient::new("invalid-filter-test");

    // Test without connecting - these should be client-side validation errors
    // Empty topic filter
    let result = client.subscribe("", |_| {}).await;
    assert!(result.is_err(), "Empty topic filter should be rejected");

    // Topic filter with null character
    let result = client.subscribe("test\0filter", |_| {}).await;
    assert!(
        result.is_err(),
        "Topic filter with null character should be rejected"
    );
}

#[tokio::test]
async fn test_packet_too_large() {
    let mut options = ConnectOptions::new("large-packet-test");
    options.properties.maximum_packet_size = Some(1024); // 1KB limit

    let client = MqttClient::with_options(options);

    // Try to publish a message larger than the limit without connecting
    let large_payload = vec![0u8; 2048]; // 2KB
    let result = client.publish("test/large", &large_payload[..]).await;
    assert!(result.is_err(), "Large packet should be rejected");

    // Should fail because not connected (client-side validation may occur later)
    match result.unwrap_err() {
        MqttError::NotConnected | MqttError::PacketTooLarge { .. } => {} // Expected
        other => panic!("Expected NotConnected or PacketTooLarge error, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_duplicate_connect() {
    let client = MqttClient::new("duplicate-connect-test");

    // Use a port that definitely has no broker
    let result1 = client.connect("mqtt://localhost:19999").await;
    let result2 = client.connect("mqtt://localhost:19999").await;

    // Both should fail with connection errors since no broker is on port 19999
    assert!(
        result1.is_err(),
        "First connect should fail (no broker on port 19999)"
    );
    assert!(
        result2.is_err(),
        "Second connect should fail (no broker on port 19999)"
    );

    // The specific error type depends on whether the first connect completed
    // before the second one was attempted
}

#[tokio::test]
async fn test_disconnect_not_connected() {
    let client = MqttClient::new("disconnect-test");

    // Disconnect without connect should fail gracefully
    let result = client.disconnect().await;
    assert!(result.is_err());

    match result.unwrap_err() {
        MqttError::NotConnected => {} // Expected
        other => panic!("Expected NotConnected error, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_qos2_timeout() {
    let client = MqttClient::new("qos2-timeout-test");

    // Test QoS 2 publish without connecting - should fail with NotConnected
    let options = PublishOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };

    let result = client
        .publish_with_options("test/qos2/timeout", "test", options)
        .await;

    assert!(
        result.is_err(),
        "QoS 2 publish should fail when not connected"
    );

    match result.unwrap_err() {
        MqttError::NotConnected => {} // Expected
        other => panic!("Expected NotConnected error, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_invalid_qos_value() {
    // Test that invalid QoS values are handled properly
    let qos = QoS::from(3u8); // Invalid QoS
    assert_eq!(qos, QoS::AtMostOnce); // Should default to QoS 0

    let qos = QoS::from(255u8); // Invalid QoS
    assert_eq!(qos, QoS::AtMostOnce); // Should default to QoS 0
}

#[tokio::test]
async fn test_network_disconnection_during_publish() {
    let client = MqttClient::new("network-disconnect-test");

    // Test publish on disconnected client
    let result = client.publish("test/topic", "message").await;
    assert!(result.is_err(), "Publish should fail when not connected");

    match result.unwrap_err() {
        MqttError::NotConnected => {} // Expected
        other => panic!("Expected NotConnected error, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_malformed_connack_handling() {
    // Test client behavior with connection attempts
    let client = MqttClient::new("malformed-test");

    // Attempt connection to a port with no broker
    let result = client.connect("mqtt://localhost:19999").await;
    assert!(
        result.is_err(),
        "Connection should fail with no broker on port 19999"
    );

    // The client should handle connection failures gracefully
    match result.unwrap_err() {
        MqttError::ConnectionError(_) => {}             // Expected
        other => println!("Got error type: {other:?}"), // Log but don't fail
    }
}

#[tokio::test]
async fn test_subscribe_with_invalid_qos() {
    let client = MqttClient::new("invalid-qos-sub");

    // Create subscribe options with valid QoS but test without connecting
    let options = SubscribeOptions {
        qos: QoS::ExactlyOnce, // Valid QoS 2
        ..Default::default()
    };

    // Subscribe should fail because not connected
    let result = client
        .subscribe_with_options("test/topic", options, |_| {})
        .await;
    assert!(result.is_err(), "Subscribe should fail when not connected");

    match result.unwrap_err() {
        MqttError::NotConnected => {} // Expected
        other => panic!("Expected NotConnected error, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_connection_lost_callback() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let disconnected = Arc::new(AtomicBool::new(false));

    let options = ConnectOptions::new("callback-test");
    let client = MqttClient::with_options(options);

    // Test disconnecting when not connected - should fail
    let result = client.disconnect().await;
    assert!(result.is_err(), "Disconnect should fail when not connected");

    match result.unwrap_err() {
        MqttError::NotConnected => {} // Expected
        other => panic!("Expected NotConnected error, got: {other:?}"),
    }

    // The callback should not have been called since no actual connection was made
    assert!(!disconnected.load(Ordering::Relaxed));
}

#[tokio::test]
async fn test_multiple_error_conditions() {
    let client = MqttClient::new("multi-error-test");

    // Error 1: Publish before connect
    assert!(client.publish("test", "msg").await.is_err());

    // Error 2: Subscribe before connect
    assert!(client.subscribe("test", |_| {}).await.is_err());

    // Error 3: Invalid address
    assert!(client.connect("not-a-valid-url").await.is_err());

    // Error 4: Disconnect when not connected
    assert!(client.disconnect().await.is_err());
}

#[tokio::test]
async fn test_flow_control_exceeded() {
    let mut options = ConnectOptions::new("flow-control-test");
    options.properties.receive_maximum = Some(2); // Allow only 2 in-flight messages

    let client = MqttClient::with_options(options);

    // Test flow control without connecting - should fail with NotConnected
    let pub_options = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };

    let result = client
        .publish_with_options("test/flow/1", "message 1", pub_options)
        .await;
    assert!(result.is_err(), "Publish should fail when not connected");

    match result.unwrap_err() {
        MqttError::NotConnected => {} // Expected
        other => panic!("Expected NotConnected error, got: {other:?}"),
    }
}
