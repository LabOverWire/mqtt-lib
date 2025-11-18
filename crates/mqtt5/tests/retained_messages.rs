// DISABLED: These tests require direct access to session state which is not
// exposed in the public API. Retained message functionality is tested through
// actual MQTT behavior in:
// - integration_complete_flow.rs (tests retained message delivery to new subscribers)
// - client_publish.rs::test_publish_retain (tests the publish_retain method)
// - message_queuing.rs::test_retained_message_queuing (tests retained message queuing)
//
// To re-enable these tests, they would need to be moved to unit tests in the session module.

/*
use mqtt5::{MqttClient, MqttError, QoS};
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::test]
async fn test_retained_message_storage_and_retrieval() {
    let client = MqttClient::new("test-retained-client");

    // Store messages received
    let received_messages = Arc::new(Mutex::new(Vec::new()));
    let received_clone = Arc::clone(&received_messages);

    // Subscribe to a topic first (to ensure callback is registered)
    let subscribe_result = client.subscribe("test/retained/topic", move |msg| {
        let received = received_clone.clone();
        tokio::spawn(async move {
            let mut msgs = received.lock().await;
            msgs.push((msg.topic.clone(), msg.payload.clone(), msg.retain));
        });
    }).await;

    // This will fail since we're not connected, but that's ok for this unit test
    assert!(subscribe_result.is_err());

    // Test retained message storage in session
    let session = client.session_state().await;

    // Create a retained message
    let packet = mqtt5::packet::publish::PublishPacket {
        topic_name: "test/retained/topic".to_string(),
        payload: b"retained message".to_vec(),
        qos: QoS::AtLeastOnce,
        retain: true,
        dup: false,
        packet_id: None,
        properties: Default::default(),
    };

    // Store the retained message
    session.store_retained_message(&packet).await;

    // Retrieve retained messages for exact topic
    let retained = session.get_retained_messages("test/retained/topic").await;
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].topic, "test/retained/topic");
    assert_eq!(retained[0].payload, b"retained message");
    assert_eq!(retained[0].qos, QoS::AtLeastOnce);

    // Clear retained message with empty payload
    let clear_packet = mqtt5::packet::publish::PublishPacket {
        topic_name: "test/retained/topic".to_string(),
        payload: vec![],
        qos: QoS::AtMostOnce,
        retain: true,
        dup: false,
        packet_id: None,
        properties: Default::default(),
    };

    session.store_retained_message(&clear_packet).await;

    // Verify message was cleared
    let retained = session.get_retained_messages("test/retained/topic").await;
    assert_eq!(retained.len(), 0);
}

#[tokio::test]
async fn test_retained_message_wildcard_matching() {
    let client = MqttClient::new("test-retained-wildcard");
    let session = client.session_state().await;

    // Store multiple retained messages
    let topics = vec![
        ("home/room1/temperature", "20.5".as_bytes()),
        ("home/room1/humidity", "65".as_bytes()),
        ("home/room2/temperature", "22.0".as_bytes()),
        ("home/room2/humidity", "60".as_bytes()),
        ("office/room1/temperature", "21.0".as_bytes()),
    ];

    for (topic, payload) in topics {
        let packet = mqtt5::packet::publish::PublishPacket {
            topic_name: topic.to_string(),
            payload: payload.to_vec(),
            qos: QoS::AtMostOnce,
            retain: true,
            dup: false,
            packet_id: None,
            properties: Default::default(),
        };
        session.store_retained_message(&packet).await;
    }

    // Test single-level wildcard
    let retained = session.get_retained_messages("home/+/temperature").await;
    assert_eq!(retained.len(), 2);
    assert!(retained.iter().any(|m| m.topic == "home/room1/temperature"));
    assert!(retained.iter().any(|m| m.topic == "home/room2/temperature"));

    // Test multi-level wildcard
    let retained = session.get_retained_messages("home/#").await;
    assert_eq!(retained.len(), 4);

    // Test specific topic
    let retained = session.get_retained_messages("office/room1/temperature").await;
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].payload, "21.0".as_bytes());
}

#[tokio::test]
async fn test_retained_message_qos_preservation() {
    let client = MqttClient::new("test-retained-qos");
    let session = client.session_state().await;

    // Store retained messages with different QoS levels
    let qos_levels = vec![
        (QoS::AtMostOnce, "test/qos0"),
        (QoS::AtLeastOnce, "test/qos1"),
        (QoS::ExactlyOnce, "test/qos2"),
    ];

    for (qos, topic) in qos_levels {
        let packet = mqtt5::packet::publish::PublishPacket {
            topic_name: topic.to_string(),
            payload: format!("QoS {:?} message", qos).into_bytes(),
            qos,
            retain: true,
            dup: false,
            packet_id: None,
            properties: Default::default(),
        };
        session.store_retained_message(&packet).await;
    }

    // Verify QoS is preserved
    let retained = session.get_retained_messages("test/qos0").await;
    assert_eq!(retained[0].qos, QoS::AtMostOnce);

    let retained = session.get_retained_messages("test/qos1").await;
    assert_eq!(retained[0].qos, QoS::AtLeastOnce);

    let retained = session.get_retained_messages("test/qos2").await;
    assert_eq!(retained[0].qos, QoS::ExactlyOnce);
}

#[tokio::test]
async fn test_publish_retain_method() {
    let client = MqttClient::new("test-publish-retain");

    // Try to publish retained message (will fail without connection)
    let result = client.publish_retain("test/topic", "retained data").await;
    assert!(matches!(result, Err(MqttError::NotConnected)));
}

#[tokio::test]
async fn test_retained_message_store_isolation() {
    // Create two separate clients
    let client1 = MqttClient::new("client1");
    let client2 = MqttClient::new("client2");

    let session1 = client1.session_state().await;
    let session2 = client2.session_state().await;

    // Store retained message in client1's session
    let packet = mqtt5::packet::publish::PublishPacket {
        topic_name: "test/isolated".to_string(),
        payload: b"client1 message".to_vec(),
        qos: QoS::AtMostOnce,
        retain: true,
        dup: false,
        packet_id: None,
        properties: Default::default(),
    };
    session1.store_retained_message(&packet).await;

    // Verify client1 has the message
    let retained1 = session1.get_retained_messages("test/isolated").await;
    assert_eq!(retained1.len(), 1);

    // Verify client2 does NOT have the message (sessions are isolated)
    let retained2 = session2.get_retained_messages("test/isolated").await;
    assert_eq!(retained2.len(), 0);
}

#[tokio::test]
async fn test_retained_message_properties() {
    let client = MqttClient::new("test-retained-props");
    let session = client.session_state().await;

    // Create a retained message with properties
    let mut properties = mqtt5::protocol::v5::properties::Properties::default();
    let _ = properties.add(
        mqtt5::protocol::v5::properties::PropertyId::MessageExpiryInterval,
        mqtt5::protocol::v5::properties::PropertyValue::FourByteInteger(3600),
    );

    let packet = mqtt5::packet::publish::PublishPacket {
        topic_name: "test/with/properties".to_string(),
        payload: b"message with props".to_vec(),
        qos: QoS::AtLeastOnce,
        retain: true,
        dup: false,
        packet_id: None,
        properties,
    };

    session.store_retained_message(&packet).await;

    // Retrieve and verify properties are preserved
    let retained = session.get_retained_messages("test/with/properties").await;
    assert_eq!(retained.len(), 1);

    let msg_expiry = retained[0].properties.get(
        mqtt5::protocol::v5::properties::PropertyId::MessageExpiryInterval
    );
    assert!(msg_expiry.is_some());
}
*/
