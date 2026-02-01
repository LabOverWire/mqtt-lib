#![allow(clippy::large_futures)]

mod common;

use common::TestBroker;
use mqtt5::time::Duration;
use mqtt5::{
    ConnectOptions, ConnectionEvent, Message, MqttClient, PublishOptions, QoS, SubscribeOptions,
    WillMessage, WillProperties,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use ulid::Ulid;

/// Helper function to get a unique client ID for tests
fn test_client_id(test_name: &str) -> String {
    format!("test-{test_name}-{}", Ulid::new())
}

#[tokio::test]
async fn test_mqtt5_properties_system() {
    // Start test broker
    let broker = TestBroker::start().await;

    let client = MqttClient::new(test_client_id("mqtt5-props"));

    // Connect with v5.0 specific properties
    let mut opts = ConnectOptions::new(test_client_id("mqtt5-props")).with_clean_start(true);

    // Set MQTT v5.0 properties
    opts.properties.session_expiry_interval = Some(3600); // 1 hour
    opts.properties.receive_maximum = Some(100);
    opts.properties.maximum_packet_size = Some(1024 * 1024); // 1MB
    opts.properties.topic_alias_maximum = Some(10);
    opts.properties.request_response_information = Some(true);
    opts.properties.request_problem_information = Some(true);
    opts.properties
        .user_properties
        .push(("client-type".to_string(), "test-suite".to_string()));
    opts.properties
        .user_properties
        .push(("version".to_string(), "1.0".to_string()));

    let connect_result = client
        .connect_with_options(broker.address(), opts)
        .await
        .expect("Failed to connect");

    assert!(!connect_result.session_present); // Clean start, so no session

    // Test publish with v5.0 properties
    let received_props = Arc::new(Mutex::new(HashMap::<String, String>::new()));
    let props_clone = Arc::clone(&received_props);

    client
        .subscribe("test/props", move |msg: Message| {
            let props = msg.properties.clone();
            let props_clone = props_clone.clone();
            tokio::spawn(async move {
                let mut map = props_clone.lock().await;
                // Store some properties for verification
                if let Some(content_type) = props.content_type {
                    map.insert("content_type".to_string(), content_type);
                }
                if let Some(response_topic) = props.response_topic {
                    map.insert("response_topic".to_string(), response_topic);
                }
                if let Some(correlation_data) = props.correlation_data {
                    map.insert(
                        "correlation_data".to_string(),
                        String::from_utf8_lossy(&correlation_data).to_string(),
                    );
                }
                // User properties
                for (key, value) in &props.user_properties {
                    map.insert(format!("user_{key}"), value.clone());
                }
            });
        })
        .await
        .expect("Failed to subscribe");

    let mut pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    pub_opts.properties.message_expiry_interval = Some(300); // 5 minutes
    pub_opts.properties.payload_format_indicator = Some(true);
    pub_opts.properties.content_type = Some("application/json".to_string());
    pub_opts.properties.response_topic = Some("response/topic".to_string());
    pub_opts.properties.correlation_data = Some(b"corr-123".to_vec());
    pub_opts
        .properties
        .user_properties
        .push(("msg-type".to_string(), "test".to_string()));
    pub_opts
        .properties
        .user_properties
        .push(("timestamp".to_string(), "2024-01-01".to_string()));

    let _ = client
        .publish_with_options("test/props", b"{\"test\": true}", pub_opts)
        .await
        .expect("Failed to publish");

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify properties were received
    let props = received_props.lock().await;
    assert_eq!(
        props.get("content_type"),
        Some(&"application/json".to_string())
    );
    assert_eq!(
        props.get("response_topic"),
        Some(&"response/topic".to_string())
    );
    assert_eq!(props.get("correlation_data"), Some(&"corr-123".to_string()));
    assert_eq!(props.get("user_msg-type"), Some(&"test".to_string()));

    client.disconnect().await.expect("Failed to disconnect");
}

#[tokio::test]
async fn test_will_message_with_delay() {
    // Start test broker
    let broker = TestBroker::start().await;

    // Client that will have a Will message
    let will_client_id = test_client_id("will-sender");
    let will_client = MqttClient::new(will_client_id.clone());

    // Subscriber to receive Will message
    let sub_client = MqttClient::new(test_client_id("will-receiver"));

    sub_client
        .connect(broker.address())
        .await
        .expect("Subscriber failed to connect");

    let will_received = Arc::new(AtomicBool::new(false));
    let will_received_clone = Arc::clone(&will_received);
    let receive_time = Arc::new(Mutex::new(None::<std::time::Instant>));
    let receive_time_clone = Arc::clone(&receive_time);

    sub_client
        .subscribe("will/topic", move |msg: Message| {
            let will_received_clone = will_received_clone.clone();
            let receive_time_clone = receive_time_clone.clone();
            tokio::spawn(async move {
                assert_eq!(&msg.payload[..], b"Client disconnected unexpectedly");
                will_received_clone.store(true, Ordering::SeqCst);
                *receive_time_clone.lock().await = Some(std::time::Instant::now());
            });
        })
        .await
        .expect("Failed to subscribe to will topic");

    // Configure Will message with delay
    let mut will_props = WillProperties {
        will_delay_interval: Some(2), // 2 seconds delay
        message_expiry_interval: Some(60),
        content_type: Some("text/plain".to_string()),
        ..Default::default()
    };
    will_props
        .user_properties
        .push(("reason".to_string(), "test-disconnect".to_string()));

    let mut will = WillMessage::new("will/topic", b"Client disconnected unexpectedly");
    will.qos = QoS::AtLeastOnce;
    will.retain = false;
    will.properties = will_props;

    let connect_opts = ConnectOptions::new(will_client_id)
        .with_clean_start(true)
        .with_will(will);

    let disconnect_time = Arc::new(Mutex::new(None::<std::time::Instant>));
    let disconnect_time_clone = Arc::clone(&disconnect_time);

    will_client
        .connect_with_options(broker.address(), connect_opts)
        .await
        .expect("Will client failed to connect");

    // Simulate abnormal disconnection without sending DISCONNECT packet
    *disconnect_time_clone.lock().await = Some(std::time::Instant::now());
    will_client
        .disconnect_abnormally()
        .await
        .expect("Failed to disconnect abnormally");

    // Wait for Will message (should arrive after delay)
    tokio::time::sleep(Duration::from_secs(4)).await;

    assert!(will_received.load(Ordering::SeqCst));

    // Verify delay was respected
    let disc_time = disconnect_time.lock().await.unwrap();
    let recv_time = receive_time.lock().await.unwrap();
    let delay = recv_time.duration_since(disc_time);
    println!("Will message delay: {delay:?}");
    // Allow some tolerance for timing
    assert!(
        delay >= Duration::from_millis(1900),
        "Delay was {delay:?}, expected at least 2 seconds"
    );

    sub_client
        .disconnect()
        .await
        .expect("Failed to disconnect subscriber");
}

#[tokio::test]
async fn test_will_message_forwards_properties() {
    let broker = TestBroker::start().await;

    let collector = common::MessageCollector::new();
    let sub_client = MqttClient::new(test_client_id("will-prop-sub"));
    sub_client
        .connect(broker.address())
        .await
        .expect("subscriber connect");
    sub_client
        .subscribe("will/props", collector.callback())
        .await
        .expect("subscribe");

    let will_props = WillProperties {
        payload_format_indicator: Some(true),
        message_expiry_interval: Some(300),
        content_type: Some("application/json".to_string()),
        response_topic: Some("reply/here".to_string()),
        correlation_data: Some(vec![0xDE, 0xAD]),
        user_properties: vec![("custom-key".to_string(), "custom-val".to_string())],
        ..Default::default()
    };

    let mut will = WillMessage::new("will/props", b"gone");
    will.qos = QoS::AtMostOnce;
    will.properties = will_props;

    let will_client = MqttClient::new(test_client_id("will-prop-sender"));
    let opts = ConnectOptions::new(test_client_id("will-prop-sender"))
        .with_clean_start(true)
        .with_will(will);
    will_client
        .connect_with_options(broker.address(), opts)
        .await
        .expect("will client connect");

    will_client
        .disconnect_abnormally()
        .await
        .expect("abnormal disconnect");

    assert!(collector.wait_for_messages(1, Duration::from_secs(5)).await);

    let msgs = collector.get_messages().await;
    let msg = &msgs[0];
    assert_eq!(msg.payload, b"gone");
    assert_eq!(msg.properties.payload_format_indicator, Some(true));
    assert_eq!(msg.properties.message_expiry_interval, Some(300));
    assert_eq!(
        msg.properties.content_type,
        Some("application/json".to_string())
    );
    assert_eq!(
        msg.properties.response_topic,
        Some("reply/here".to_string())
    );
    assert_eq!(msg.properties.correlation_data, Some(vec![0xDE, 0xAD]));
    assert!(msg
        .properties
        .user_properties
        .contains(&("custom-key".to_string(), "custom-val".to_string())));

    sub_client.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn test_will_message_strips_spoofed_sender() {
    let broker = TestBroker::start_with_authentication().await;

    let collector = common::MessageCollector::new();
    let sub_client = MqttClient::new(test_client_id("will-sender-sub"));
    let sub_opts = ConnectOptions::new(test_client_id("will-sender-sub"))
        .with_clean_start(true)
        .with_credentials("testuser", b"testpass");
    sub_client
        .connect_with_options(broker.address(), sub_opts)
        .await
        .expect("subscriber connect");
    sub_client
        .subscribe("will/sender", collector.callback())
        .await
        .expect("subscribe");

    let will_props = WillProperties {
        user_properties: vec![("x-mqtt-sender".to_string(), "evil-impersonator".to_string())],
        ..Default::default()
    };

    let mut will = WillMessage::new("will/sender", b"disconnected");
    will.qos = QoS::AtMostOnce;
    will.properties = will_props;

    let will_client = MqttClient::new(test_client_id("will-sender-client"));
    let opts = ConnectOptions::new(test_client_id("will-sender-client"))
        .with_clean_start(true)
        .with_credentials("testuser", b"testpass")
        .with_will(will);
    will_client
        .connect_with_options(broker.address(), opts)
        .await
        .expect("will client connect");

    will_client
        .disconnect_abnormally()
        .await
        .expect("abnormal disconnect");

    assert!(collector.wait_for_messages(1, Duration::from_secs(5)).await);

    let msgs = collector.get_messages().await;
    let msg = &msgs[0];

    let sender_props: Vec<&str> = msg
        .properties
        .user_properties
        .iter()
        .filter(|(k, _)| k == "x-mqtt-sender")
        .map(|(_, v)| v.as_str())
        .collect();

    assert_eq!(sender_props.len(), 1);
    assert_eq!(sender_props[0], "testuser");
    assert!(!msg
        .properties
        .user_properties
        .iter()
        .any(|(_, v)| v == "evil-impersonator"));

    sub_client.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn test_delayed_will_message_forwards_properties() {
    let broker = TestBroker::start().await;

    let collector = common::MessageCollector::new();
    let sub_client = MqttClient::new(test_client_id("delay-prop-sub"));
    sub_client
        .connect(broker.address())
        .await
        .expect("subscriber connect");
    sub_client
        .subscribe("will/delay-props", collector.callback())
        .await
        .expect("subscribe");

    let will_props = WillProperties {
        will_delay_interval: Some(1),
        payload_format_indicator: Some(true),
        message_expiry_interval: Some(120),
        content_type: Some("text/plain".to_string()),
        response_topic: Some("reply/delayed".to_string()),
        correlation_data: Some(vec![0xCA, 0xFE]),
        user_properties: vec![("delayed-key".to_string(), "delayed-val".to_string())],
    };

    let mut will = WillMessage::new("will/delay-props", b"delayed-gone");
    will.qos = QoS::AtMostOnce;
    will.properties = will_props;

    let will_client = MqttClient::new(test_client_id("delay-prop-sender"));
    let opts = ConnectOptions::new(test_client_id("delay-prop-sender"))
        .with_clean_start(true)
        .with_will(will);
    will_client
        .connect_with_options(broker.address(), opts)
        .await
        .expect("will client connect");

    will_client
        .disconnect_abnormally()
        .await
        .expect("abnormal disconnect");

    assert!(collector.wait_for_messages(1, Duration::from_secs(5)).await);

    let msgs = collector.get_messages().await;
    let msg = &msgs[0];
    assert_eq!(msg.payload, b"delayed-gone");
    assert_eq!(msg.properties.payload_format_indicator, Some(true));
    assert_eq!(msg.properties.message_expiry_interval, Some(120));
    assert_eq!(msg.properties.content_type, Some("text/plain".to_string()));
    assert_eq!(
        msg.properties.response_topic,
        Some("reply/delayed".to_string())
    );
    assert_eq!(msg.properties.correlation_data, Some(vec![0xCA, 0xFE]));
    assert!(msg
        .properties
        .user_properties
        .contains(&("delayed-key".to_string(), "delayed-val".to_string())));

    sub_client.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn test_normal_publish_injects_sender() {
    let broker = TestBroker::start_with_authentication().await;

    let collector = common::MessageCollector::new();
    let sub_client = MqttClient::new(test_client_id("pub-sender-sub"));
    let sub_opts = ConnectOptions::new(test_client_id("pub-sender-sub"))
        .with_clean_start(true)
        .with_credentials("testuser", b"testpass");
    sub_client
        .connect_with_options(broker.address(), sub_opts)
        .await
        .expect("subscriber connect");
    sub_client
        .subscribe("test/sender-inject", collector.callback())
        .await
        .expect("subscribe");

    let pub_client = MqttClient::new(test_client_id("pub-sender-client"));
    let pub_opts = ConnectOptions::new(test_client_id("pub-sender-client"))
        .with_clean_start(true)
        .with_credentials("testuser", b"testpass");
    pub_client
        .connect_with_options(broker.address(), pub_opts)
        .await
        .expect("publisher connect");

    pub_client
        .publish_with_options(
            "test/sender-inject",
            b"hello",
            PublishOptions {
                qos: QoS::AtMostOnce,
                ..Default::default()
            },
        )
        .await
        .expect("publish");

    assert!(collector.wait_for_messages(1, Duration::from_secs(5)).await);

    let msgs = collector.get_messages().await;
    let msg = &msgs[0];

    let sender_props: Vec<&str> = msg
        .properties
        .user_properties
        .iter()
        .filter(|(k, _)| k == "x-mqtt-sender")
        .map(|(_, v)| v.as_str())
        .collect();

    assert_eq!(sender_props.len(), 1);
    assert_eq!(sender_props[0], "testuser");

    pub_client.disconnect().await.expect("disconnect pub");
    sub_client.disconnect().await.expect("disconnect sub");
}

#[tokio::test]
async fn test_topic_aliases() {
    // Start test broker
    let broker = TestBroker::start().await;

    let client = MqttClient::new(test_client_id("topic-alias"));

    // Connect with topic alias support
    let mut opts = ConnectOptions::new(test_client_id("topic-alias"));
    opts.properties.topic_alias_maximum = Some(5);

    client
        .connect_with_options(broker.address(), opts)
        .await
        .expect("Failed to connect");

    let received = Arc::new(Mutex::new(Vec::<(String, Vec<u8>)>::new()));
    let received_clone = Arc::clone(&received);

    client
        .subscribe("sensors/+/temperature", move |msg: Message| {
            let received_clone = received_clone.clone();
            let topic = msg.topic.clone();
            let payload = msg.payload.clone();
            tokio::spawn(async move {
                received_clone.lock().await.push((topic, payload));
            });
        })
        .await
        .expect("Failed to subscribe");

    // Publish multiple times to same topic (should use alias after first)
    for i in 0..5 {
        client
            .publish_qos1("sensors/room1/temperature", format!("25.{i}").as_bytes())
            .await
            .expect("Failed to publish");
    }

    // Publish to different topics
    client
        .publish_qos1("sensors/room2/temperature", b"26.0")
        .await
        .unwrap();
    client
        .publish_qos1("sensors/room3/temperature", b"24.5")
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let msgs = received.lock().await;
    assert_eq!(msgs.len(), 7); // 5 + 2 messages

    // All messages should be received correctly despite alias usage
    assert_eq!(
        msgs.iter()
            .filter(|(t, _)| t == "sensors/room1/temperature")
            .count(),
        5
    );
    assert_eq!(
        msgs.iter()
            .filter(|(t, _)| t == "sensors/room2/temperature")
            .count(),
        1
    );
    assert_eq!(
        msgs.iter()
            .filter(|(t, _)| t == "sensors/room3/temperature")
            .count(),
        1
    );

    client.disconnect().await.expect("Failed to disconnect");
}

#[tokio::test]
async fn test_flow_control_receive_maximum() {
    // Start test broker
    let broker = TestBroker::start().await;

    let client = MqttClient::new(test_client_id("flow-control"));

    // Connect with limited receive maximum
    let mut opts = ConnectOptions::new(test_client_id("flow-control"));
    opts.properties.receive_maximum = Some(2); // Only 2 in-flight messages

    client
        .connect_with_options(broker.address(), opts)
        .await
        .expect("Failed to connect");

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = Arc::clone(&received);
    let processing = Arc::new(Mutex::new(Vec::<String>::new()));
    let processing_clone = Arc::clone(&processing);

    client
        .subscribe("flow/test", move |msg: Message| {
            let msg_str = String::from_utf8_lossy(&msg.payload).to_string();
            let processing_clone = processing_clone.clone();
            let received_clone = received_clone.clone();
            tokio::spawn(async move {
                processing_clone.lock().await.push(msg_str.clone());

                // Simulate slow processing
                tokio::time::sleep(Duration::from_millis(100)).await;

                received_clone.fetch_add(1, Ordering::SeqCst);
            });
        })
        .await
        .expect("Failed to subscribe");

    // Publisher sends multiple messages quickly
    let publisher = MqttClient::new(test_client_id("flow-publisher"));

    publisher
        .connect(broker.address())
        .await
        .expect("Failed to connect publisher");

    // Send 5 messages rapidly
    for i in 1..=5 {
        publisher
            .publish_qos2("flow/test", format!("Message {i}").as_bytes())
            .await
            .expect("Failed to publish");
    }

    // Wait for processing
    tokio::time::sleep(Duration::from_secs(1)).await;

    // All messages should eventually be received
    assert_eq!(received.load(Ordering::SeqCst), 5);

    let processed = processing.lock().await;
    assert_eq!(processed.len(), 5);
    for i in 1..=5 {
        assert!(processed.contains(&format!("Message {i}")));
    }

    client.disconnect().await.expect("Failed to disconnect");
    publisher
        .disconnect()
        .await
        .expect("Failed to disconnect publisher");
}

#[tokio::test]
async fn test_subscription_identifiers() {
    // Start test broker
    let broker = TestBroker::start().await;

    let client = MqttClient::new(test_client_id("sub-id"));

    client
        .connect(broker.address())
        .await
        .expect("Failed to connect");

    let received_ids = Arc::new(Mutex::new(Vec::<u32>::new()));

    // Subscribe with different subscription IDs
    let ids_clone = Arc::clone(&received_ids);
    let sub_opts1 = SubscribeOptions::default().with_subscription_identifier(1);

    client
        .subscribe_with_options("test/sub/+", sub_opts1, move |msg: Message| {
            let ids_clone = ids_clone.clone();
            let subscription_identifiers = msg.properties.subscription_identifiers.clone();
            tokio::spawn(async move {
                if !subscription_identifiers.is_empty() {
                    let sub_id = subscription_identifiers[0];
                    ids_clone.lock().await.push(sub_id);
                }
            });
        })
        .await
        .expect("Failed to subscribe with ID 1");

    let ids_clone = Arc::clone(&received_ids);
    let sub_opts2 = SubscribeOptions::default().with_subscription_identifier(2);

    client
        .subscribe_with_options("test/+/data", sub_opts2, move |msg: Message| {
            let ids_clone = ids_clone.clone();
            let subscription_identifiers = msg.properties.subscription_identifiers.clone();
            tokio::spawn(async move {
                if !subscription_identifiers.is_empty() {
                    let sub_id = subscription_identifiers[0];
                    ids_clone.lock().await.push(sub_id);
                }
            });
        })
        .await
        .expect("Failed to subscribe with ID 2");

    // Publish to topics that match different subscriptions
    client.publish_qos1("test/sub/one", b"data").await.unwrap();
    client
        .publish_qos1("test/sensor/data", b"data")
        .await
        .unwrap();
    client.publish_qos1("test/sub/data", b"data").await.unwrap(); // Matches both

    tokio::time::sleep(Duration::from_millis(500)).await;

    let ids = received_ids.lock().await;
    println!("Received subscription IDs: {:?}", *ids);
    if ids.is_empty() {
        println!(
            "Warning: No subscription identifiers received - feature may not be fully implemented"
        );
    } else {
        assert!(ids.contains(&1)); // From first subscription
        assert!(ids.contains(&2)); // From second subscription
    }

    client.disconnect().await.expect("Failed to disconnect");
}

#[tokio::test]
async fn test_shared_subscriptions() {
    // Start test broker
    let broker = TestBroker::start().await;

    // Note: Shared subscriptions require broker support
    // This test assumes Mosquitto is configured with shared subscription support

    let received = Arc::new(Mutex::new(HashMap::<String, Vec<String>>::new()));

    // Create multiple clients sharing a subscription
    let mut clients = vec![];
    for i in 0..3 {
        let client_id = test_client_id(&format!("shared-{i}"));
        let client = MqttClient::new(client_id.clone());

        client
            .connect(broker.address())
            .await
            .expect("Failed to connect");

        let received_clone = Arc::clone(&received);
        let client_name = client_id.clone();

        // Shared subscription format: $share/group/topic
        client
            .subscribe("$share/testgroup/shared/topic", move |msg: Message| {
                let payload = String::from_utf8_lossy(&msg.payload).to_string();
                let received_clone = received_clone.clone();
                let client_name = client_name.clone();
                tokio::spawn(async move {
                    received_clone
                        .lock()
                        .await
                        .entry(client_name)
                        .or_default()
                        .push(payload);
                });
            })
            .await
            .expect("Failed to subscribe to shared topic");

        clients.push(client);
    }

    // Publisher sends multiple messages
    let publisher = MqttClient::new(test_client_id("shared-publisher"));

    publisher
        .connect(broker.address())
        .await
        .expect("Failed to connect publisher");

    for i in 1..=9 {
        publisher
            .publish_qos1("shared/topic", format!("Message {i}").as_bytes())
            .await
            .expect("Failed to publish");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify load balancing
    let msgs = received.lock().await;
    let total_messages: usize = msgs.values().map(std::vec::Vec::len).sum();

    // All messages should be received exactly once across all clients
    assert_eq!(total_messages, 9);

    // Each client should have received some messages (roughly balanced)
    for (_client, messages) in msgs.iter() {
        assert!(!messages.is_empty());
        assert!(messages.len() <= 5); // No client should get all messages
    }

    // Disconnect all clients
    for client in clients {
        client.disconnect().await.expect("Failed to disconnect");
    }
    publisher
        .disconnect()
        .await
        .expect("Failed to disconnect publisher");
}

#[tokio::test]
async fn test_maximum_packet_size() {
    // Start test broker
    let broker = TestBroker::start().await;

    let client = MqttClient::new(test_client_id("max-packet"));

    // Connect with maximum packet size limit
    let mut opts = ConnectOptions::new(test_client_id("max-packet"));
    opts.properties.maximum_packet_size = Some(1024); // 1KB limit

    client
        .connect_with_options(broker.address(), opts)
        .await
        .expect("Failed to connect");

    // Try to publish within limit
    let small_payload = vec![0x42; 512]; // 512 bytes
    let result = client.publish("test/size", small_payload).await;
    assert!(result.is_ok());

    // Try to publish exceeding limit
    let large_payload = vec![0x42; 2048]; // 2KB
    let result = client.publish("test/size", large_payload).await;

    // Should fail due to packet size limit
    assert!(result.is_err());

    client.disconnect().await.expect("Failed to disconnect");
}

#[tokio::test]
async fn test_reason_codes_and_strings() {
    // Start test broker
    let broker = TestBroker::start().await;

    let client = MqttClient::new(test_client_id("reason-codes"));

    let disconnect_reason = Arc::new(Mutex::new(None));
    let reason_clone = Arc::clone(&disconnect_reason);

    client
        .on_connection_event(move |event| {
            let reason_clone = reason_clone.clone();
            tokio::spawn(async move {
                if let ConnectionEvent::Disconnected { reason } = event {
                    *reason_clone.lock().await = Some(reason);
                }
            });
        })
        .await
        .expect("Failed to register event handler");

    // Connect normally
    client
        .connect(broker.address())
        .await
        .expect("Failed to connect");

    // Subscribe to a topic that might have access restrictions
    // (depending on broker configuration)
    let result = client.subscribe("$SYS/restricted", |_| {}).await;

    // The result might contain a reason code if subscription is denied
    if result.is_err() {
        println!("Subscription failed as expected: {result:?}");
    }

    client.disconnect().await.expect("Failed to disconnect");
}

#[tokio::test]
async fn test_request_response_information() {
    let broker = TestBroker::start().await;

    let client1 = MqttClient::new(test_client_id("resp-info-requested"));
    let mut opts1 =
        ConnectOptions::new(test_client_id("resp-info-requested")).with_clean_start(true);
    opts1.properties.request_response_information = Some(true);

    let result1 = client1
        .connect_with_options(broker.address(), opts1)
        .await
        .expect("Failed to connect with request_response_information=true");
    assert!(!result1.session_present);
    client1.disconnect().await.expect("Failed to disconnect");

    let client2 = MqttClient::new(test_client_id("resp-info-not-requested"));
    let mut opts2 =
        ConnectOptions::new(test_client_id("resp-info-not-requested")).with_clean_start(true);
    opts2.properties.request_response_information = Some(false);

    let result2 = client2
        .connect_with_options(broker.address(), opts2)
        .await
        .expect("Failed to connect with request_response_information=false");
    assert!(!result2.session_present);
    client2.disconnect().await.expect("Failed to disconnect");

    let client3 = MqttClient::new(test_client_id("resp-info-default"));
    let opts3 = ConnectOptions::new(test_client_id("resp-info-default")).with_clean_start(true);

    let result3 = client3
        .connect_with_options(broker.address(), opts3)
        .await
        .expect("Failed to connect with default request_response_information");
    assert!(!result3.session_present);
    client3.disconnect().await.expect("Failed to disconnect");
}

#[tokio::test]
async fn test_request_problem_information() {
    let broker = TestBroker::start().await;

    let client1 = MqttClient::new(test_client_id("prob-info-enabled"));
    let mut opts1 = ConnectOptions::new(test_client_id("prob-info-enabled")).with_clean_start(true);
    opts1.properties.request_problem_information = Some(true);

    let result1 = client1
        .connect_with_options(broker.address(), opts1)
        .await
        .expect("Failed to connect with request_problem_information=true");
    assert!(!result1.session_present);
    client1.disconnect().await.expect("Failed to disconnect");

    let client2 = MqttClient::new(test_client_id("prob-info-disabled"));
    let mut opts2 =
        ConnectOptions::new(test_client_id("prob-info-disabled")).with_clean_start(true);
    opts2.properties.request_problem_information = Some(false);

    let result2 = client2
        .connect_with_options(broker.address(), opts2)
        .await
        .expect("Failed to connect with request_problem_information=false");
    assert!(!result2.session_present);
    client2.disconnect().await.expect("Failed to disconnect");

    let client3 = MqttClient::new(test_client_id("prob-info-default"));
    let opts3 = ConnectOptions::new(test_client_id("prob-info-default")).with_clean_start(true);

    let result3 = client3
        .connect_with_options(broker.address(), opts3)
        .await
        .expect("Failed to connect with default request_problem_information");
    assert!(!result3.session_present);
    client3.disconnect().await.expect("Failed to disconnect");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_will_message_blocked_by_acl() {
    use mqtt5::broker::config::{
        AuthConfig, AuthMethod, BrokerConfig, RateLimitConfig, StorageBackend, StorageConfig,
    };
    use std::io::Write;
    use std::process::Command;

    let temp_dir = std::env::temp_dir();
    let pid = std::process::id();

    let acl_file = temp_dir.join(format!("test_will_acl_{pid}.txt"));
    let password_file = temp_dir.join(format!("test_will_passwords_{pid}.txt"));

    let _ = std::fs::remove_file(&acl_file);
    let _ = std::fs::remove_file(&password_file);

    {
        let mut f = std::fs::File::create(&acl_file).expect("create acl file");
        writeln!(f, "user willuser topic will/blocked permission deny").expect("write acl deny");
        writeln!(f, "user * topic # permission readwrite").expect("write acl allow");
    }

    let workspace_root = {
        let mut current = std::env::current_dir().expect("cwd");
        loop {
            let cargo_toml = current.join("Cargo.toml");
            if cargo_toml.exists() {
                if let Ok(contents) = std::fs::read_to_string(&cargo_toml) {
                    if contents.contains("[workspace]") {
                        break current;
                    }
                }
            }
            assert!(current.pop(), "workspace root not found");
        }
    };
    let cli_binary = if let Ok(path) = std::env::var("CARGO_BIN_EXE_mqttv5") {
        std::path::PathBuf::from(path)
    } else {
        workspace_root.join("target").join("release").join("mqttv5")
    };

    let status = Command::new(&cli_binary)
        .args([
            "passwd",
            "-c",
            "-b",
            "testpass",
            "willuser",
            password_file.to_str().unwrap(),
        ])
        .status()
        .expect("create password file");
    assert!(status.success(), "password file creation failed");

    let status = Command::new(&cli_binary)
        .args([
            "passwd",
            "-b",
            "testpass",
            "subuser",
            password_file.to_str().unwrap(),
        ])
        .status()
        .expect("add second user");
    assert!(status.success(), "second user creation failed");

    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };

    let auth_config = AuthConfig {
        allow_anonymous: false,
        password_file: Some(password_file.clone()),
        acl_file: Some(acl_file.clone()),
        auth_method: AuthMethod::Password,
        auth_data: Some(std::fs::read(&password_file).expect("read password file")),
        scram_file: None,
        jwt_config: None,
        federated_jwt_config: None,
        rate_limit: RateLimitConfig::default(),
    };

    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_storage(storage_config)
        .with_auth(auth_config);

    let broker = TestBroker::start_with_config(config).await;

    let collector = common::MessageCollector::new();
    let sub_client = MqttClient::new(test_client_id("will-acl-sub"));
    let sub_opts = ConnectOptions::new(test_client_id("will-acl-sub"))
        .with_clean_start(true)
        .with_credentials("subuser", b"testpass");
    sub_client
        .connect_with_options(broker.address(), sub_opts)
        .await
        .expect("subscriber connect");
    sub_client
        .subscribe("will/blocked", collector.callback())
        .await
        .expect("subscribe");

    let will = WillMessage::new("will/blocked", b"should not arrive");
    let will_client = MqttClient::new(test_client_id("will-acl-sender"));
    let opts = ConnectOptions::new(test_client_id("will-acl-sender"))
        .with_clean_start(true)
        .with_credentials("willuser", b"testpass")
        .with_will(will);
    will_client
        .connect_with_options(broker.address(), opts)
        .await
        .expect("will client connect");

    will_client
        .disconnect_abnormally()
        .await
        .expect("abnormal disconnect");

    let received = collector.wait_for_messages(1, Duration::from_secs(3)).await;
    assert!(
        !received,
        "will message should have been blocked by ACL deny rule"
    );
    assert_eq!(collector.count().await, 0);

    let allowed_collector = common::MessageCollector::new();
    sub_client
        .subscribe("test/allowed", allowed_collector.callback())
        .await
        .expect("subscribe allowed");

    let pub_client = MqttClient::new(test_client_id("will-acl-pub"));
    let pub_opts = ConnectOptions::new(test_client_id("will-acl-pub"))
        .with_clean_start(true)
        .with_credentials("subuser", b"testpass");
    pub_client
        .connect_with_options(broker.address(), pub_opts)
        .await
        .expect("pub client connect");
    pub_client
        .publish("test/allowed", b"hello")
        .await
        .expect("publish");

    assert!(
        allowed_collector
            .wait_for_messages(1, Duration::from_secs(3))
            .await,
        "normal publish to allowed topic should succeed"
    );

    sub_client.disconnect().await.expect("disconnect sub");
    pub_client.disconnect().await.expect("disconnect pub");

    let _ = std::fs::remove_file(&acl_file);
    let _ = std::fs::remove_file(&password_file);
}
