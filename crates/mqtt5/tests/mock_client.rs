//! Tests for the mock MQTT client functionality

use mqtt5::{MockCall, MockMqttClient, MqttClientTrait, PublishResult, QoS};

#[tokio::test]
async fn test_mock_client_creation() {
    let mock = MockMqttClient::new("test-client");

    assert!(!mock.is_connected().await);
    assert_eq!(mock.client_id().await, "test-client");
    assert!(!mock.is_queue_on_disconnect().await);
}

#[tokio::test]
async fn test_mock_client_connect_disconnect() {
    let mock = MockMqttClient::new("test-client");

    // Test connect
    let result = mock.connect("mqtt://localhost:1883").await;
    assert!(result.is_ok());
    assert!(mock.is_connected().await);

    // Verify call was recorded
    let calls = mock.get_calls().await;
    assert_eq!(calls.len(), 1);
    assert!(matches!(
        calls[0],
        MockCall::Connect { ref address } if address == "mqtt://localhost:1883"
    ));

    // Test disconnect
    let result = mock.disconnect().await;
    assert!(result.is_ok());
    assert!(!mock.is_connected().await);

    // Verify both calls were recorded
    let calls = mock.get_calls().await;
    assert_eq!(calls.len(), 2);
    assert!(matches!(calls[1], MockCall::Disconnect));
}

#[tokio::test]
async fn test_mock_client_publish() {
    let mock = MockMqttClient::new("test-client");
    mock.set_connected(true);

    // Test QoS 0 publish
    let result = mock.publish("test/topic", b"test message").await;
    assert!(result.is_ok());
    assert!(matches!(result.unwrap(), PublishResult::QoS0));

    // Test QoS 1 publish
    let result = mock.publish_qos1("test/topic", b"test message").await;
    assert!(result.is_ok());
    if let PublishResult::QoS1Or2 { packet_id } = result.unwrap() {
        assert!(packet_id > 0);
    } else {
        panic!("Expected QoS1Or2 result");
    }

    // Verify calls were recorded
    let calls = mock.get_calls().await;
    assert_eq!(calls.len(), 2);
    assert!(matches!(
        calls[0],
        MockCall::Publish { ref topic, ref payload }
        if topic == "test/topic" && payload == b"test message"
    ));
}

#[tokio::test]
async fn test_mock_client_subscribe() {
    let mock = MockMqttClient::new("test-client");
    mock.set_connected(true);

    // Test subscribe
    let result = mock
        .subscribe("test/topic", |_msg| {
            println!("Received message in test");
        })
        .await;

    assert!(result.is_ok());
    let (packet_id, qos) = result.unwrap();
    assert!(packet_id > 0);
    assert_eq!(qos, QoS::AtMostOnce);

    // Verify call was recorded
    let calls = mock.get_calls().await;
    assert_eq!(calls.len(), 1);
    assert!(matches!(
        calls[0],
        MockCall::Subscribe { ref topic } if topic == "test/topic"
    ));
}

#[tokio::test]
async fn test_mock_client_simulate_message() {
    use std::sync::{Arc, Mutex};

    let mock = MockMqttClient::new("test-client");
    let received_messages = Arc::new(Mutex::new(Vec::new()));
    let messages_clone = Arc::clone(&received_messages);

    // Subscribe to a topic
    let result = mock
        .subscribe("test/+", move |msg| {
            messages_clone
                .lock()
                .unwrap()
                .push((msg.topic, msg.payload));
        })
        .await;
    assert!(result.is_ok());

    // Simulate a message
    let result = mock
        .simulate_message("test/hello", b"world".to_vec(), QoS::AtMostOnce)
        .await;
    assert!(result.is_ok());

    // Check that callback was called
    let messages = received_messages.lock().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].0, "test/hello");
    assert_eq!(messages[0].1, b"world");
}

#[tokio::test]
async fn test_mock_client_configured_responses() {
    let mock = MockMqttClient::new("test-client");

    // Configure a custom publish response
    mock.set_publish_response(Ok(PublishResult::QoS1Or2 { packet_id: 42 }))
        .await;

    // Test that the configured response is returned
    let result = mock.publish("test/topic", b"test").await;
    assert!(result.is_ok());
    if let PublishResult::QoS1Or2 { packet_id } = result.unwrap() {
        assert_eq!(packet_id, 42);
    } else {
        panic!("Expected configured QoS1Or2 result");
    }

    // Configure a custom subscribe response
    mock.set_subscribe_response(Ok((123, QoS::ExactlyOnce)))
        .await;

    let result = mock.subscribe("test/topic", |_| {}).await;
    assert!(result.is_ok());
    let (packet_id, qos) = result.unwrap();
    assert_eq!(packet_id, 123);
    assert_eq!(qos, QoS::ExactlyOnce);
}

#[tokio::test]
async fn test_mock_client_call_tracking() {
    let mock = MockMqttClient::new("test-client");

    // Perform various operations
    let _ = mock.connect("mqtt://localhost:1883").await;
    let _ = mock.publish("topic1", b"msg1").await;
    let _ = mock.subscribe("topic2", |_| {}).await;
    let _ = mock.unsubscribe("topic2").await;
    let _ = mock.disconnect().await;

    // Check that all calls were recorded
    let calls = mock.get_calls().await;
    assert_eq!(calls.len(), 5);

    // Verify call types
    assert!(matches!(calls[0], MockCall::Connect { .. }));
    assert!(matches!(calls[1], MockCall::Publish { .. }));
    assert!(matches!(calls[2], MockCall::Subscribe { .. }));
    assert!(matches!(calls[3], MockCall::Unsubscribe { .. }));
    assert!(matches!(calls[4], MockCall::Disconnect));

    // Clear calls and verify
    mock.clear_calls().await;
    let calls = mock.get_calls().await;
    assert_eq!(calls.len(), 0);
}
