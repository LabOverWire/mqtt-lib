use mqtt5::{ConnectOptions, MqttClient, PublishOptions, PublishResult, QoS};

#[tokio::test]

async fn test_message_queuing_when_disconnected() {
    // Create client with clean_start=false to enable queuing
    let options = ConnectOptions::new("test-client").with_clean_start(false);
    let client = MqttClient::with_options(options);

    // Queuing should be enabled for persistent sessions
    assert!(client.is_queue_on_disconnect().await);

    // Try to publish while disconnected - should queue the message
    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };

    let result = client
        .publish_with_options("test/topic", "queued message", options)
        .await;

    // Should return a packet ID even though we're not connected
    assert!(result.is_ok());
    match result.unwrap() {
        PublishResult::QoS1Or2 { packet_id } => assert!(packet_id > 0),
        PublishResult::QoS0 => panic!("Expected QoS1Or2 result"),
    }
}

#[tokio::test]
async fn test_message_queuing_disabled() {
    let options = ConnectOptions::new("test-client").with_clean_start(true);
    let client = MqttClient::with_options(options);

    // Queuing should be disabled for clean sessions
    assert!(!client.is_queue_on_disconnect().await);

    // Try to publish while disconnected - should fail
    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };

    let result = client
        .publish_with_options("test/topic", "message", options)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_qos0_not_queued() {
    let client = MqttClient::new("test-client");

    // QoS 0 messages should not be queued
    let options = PublishOptions::default(); // QoS 0 by default

    let result = client
        .publish_with_options("test/topic", "qos0 message", options)
        .await;
    assert!(result.is_err()); // Should fail with NotConnected
}

#[tokio::test]

async fn test_queue_multiple_messages() {
    // Create client with clean_start=false to enable queuing
    let options = ConnectOptions::new("test-client").with_clean_start(false);
    let client = MqttClient::with_options(options);

    let mut packet_ids = Vec::new();

    // Queue multiple messages
    for i in 0..5 {
        let options = PublishOptions {
            qos: QoS::AtLeastOnce,
            ..Default::default()
        };

        let result = client
            .publish_with_options(format!("test/topic/{i}"), format!("message {i}"), options)
            .await;

        assert!(result.is_ok());
        match result.unwrap() {
            PublishResult::QoS1Or2 { packet_id } => packet_ids.push(packet_id),
            PublishResult::QoS0 => panic!("Expected QoS1Or2 result"),
        }
    }

    // All packet IDs should be unique
    let mut unique_ids = packet_ids.clone();
    unique_ids.sort_unstable();
    unique_ids.dedup();
    assert_eq!(packet_ids.len(), unique_ids.len());
}

#[tokio::test]

async fn test_toggle_queue_on_disconnect() {
    // Create client with clean_start=false to enable queuing initially
    let options = ConnectOptions::new("test-client").with_clean_start(false);
    let client = MqttClient::with_options(options);

    // Should be enabled for persistent sessions
    assert!(client.is_queue_on_disconnect().await);

    // Disable queuing
    client.set_queue_on_disconnect(false).await;
    assert!(!client.is_queue_on_disconnect().await);

    // Try to publish - should fail
    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    let result = client
        .publish_with_options("test/topic", "message", options)
        .await;
    assert!(result.is_err());

    // Re-enable queuing
    client.set_queue_on_disconnect(true).await;
    assert!(client.is_queue_on_disconnect().await);

    // Try to publish - should succeed
    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    let result = client
        .publish_with_options("test/topic", "message", options)
        .await;
    assert!(result.is_ok());
}

#[tokio::test]

async fn test_message_replay_on_reconnect() {
    // This test would require a mock broker to verify that messages are replayed
    // For now, we just test the queueing behavior

    // Create client with clean_start=false to enable queuing
    let options = ConnectOptions::new("test-client").with_clean_start(false);
    let client = MqttClient::with_options(options);

    // Queue several messages
    let messages = vec![
        ("test/1", "message 1", QoS::AtLeastOnce),
        ("test/2", "message 2", QoS::ExactlyOnce),
        ("test/3", "message 3", QoS::AtLeastOnce),
    ];

    let mut packet_ids = Vec::new();

    for (topic, payload, qos) in messages {
        let options = PublishOptions {
            qos,
            ..Default::default()
        };

        let result = client.publish_with_options(topic, payload, options).await;
        assert!(result.is_ok());
        match result.unwrap() {
            PublishResult::QoS1Or2 { packet_id } => packet_ids.push(packet_id),
            PublishResult::QoS0 => panic!("Expected QoS1Or2 result"),
        }
    }

    assert_eq!(packet_ids.len(), 3);

    // In a real test, we would:
    // 1. Connect to a broker
    // 2. Verify that all queued messages are sent with DUP flag
    // 3. Verify that they maintain their original QoS levels
}

#[tokio::test]

async fn test_retained_message_queuing() {
    // Create client with clean_start=false to enable queuing
    let options = ConnectOptions::new("test-client").with_clean_start(false);
    let client = MqttClient::with_options(options);

    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        retain: true,
        ..Default::default()
    };

    let result = client
        .publish_with_options("test/retained", "retained message", options)
        .await;
    assert!(result.is_ok());

    // The retained flag should be preserved when the message is replayed
}

#[tokio::test]

async fn test_clean_session_no_queuing() {
    let options = ConnectOptions::new("clean-client").with_clean_start(true);
    let client = MqttClient::with_options(options);

    // Verify queuing is disabled for clean sessions
    assert!(!client.is_queue_on_disconnect().await);

    // But we can manually enable it if needed
    client.set_queue_on_disconnect(true).await;
    assert!(client.is_queue_on_disconnect().await);

    // Now queuing should work even for clean session
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };

    let result = client
        .publish_with_options("test/topic", "message", pub_opts)
        .await;
    assert!(result.is_ok());
}
