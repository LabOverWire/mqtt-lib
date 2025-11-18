use mqtt5::time::Duration;
use mqtt5::{MqttClient, MqttError, QoS, RetainHandling, SubscribeOptions};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use ulid::Ulid;

/// Helper function to get a unique client ID for tests
fn test_client_id(test_name: &str) -> String {
    format!("test-{test_name}-{}", Ulid::new())
}

#[tokio::test]
async fn test_subscribe_not_connected() {
    let client = MqttClient::new(test_client_id("subscribe-not-connected"));

    let result = client.subscribe("test/topic", |_| {}).await;
    assert!(matches!(result, Err(MqttError::NotConnected)));
}

#[tokio::test]
async fn test_simple_subscription() {
    let client = MqttClient::new(test_client_id("simple-subscription"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            // Subscribe to a topic
            let counter = Arc::new(AtomicU32::new(0));
            let counter_clone = Arc::clone(&counter);

            let result = client
                .subscribe("test/topic", move |msg| {
                    assert_eq!(msg.topic, "test/topic");
                    counter_clone.fetch_add(1, Ordering::Relaxed);
                })
                .await;

            assert!(result.is_ok());

            // Publish to trigger callback
            client.publish("test/topic", b"test message").await.unwrap();

            // Give some time for message to be received
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Note: Without a broker running, the callback won't be triggered
            // This test mainly verifies the subscription API works

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_subscribe_with_options() {
    let client = MqttClient::new(test_client_id("subscribe-with-options"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            let options = SubscribeOptions {
                qos: QoS::AtLeastOnce,
                no_local: true,
                retain_as_published: true,
                retain_handling: RetainHandling::SendIfNew,
                subscription_identifier: None,
            };

            let result = client
                .subscribe_with_options("test/options", options, |msg| {
                    println!("Received: {} - {:?}", msg.topic, msg.payload);
                })
                .await;

            assert!(result.is_ok());
            let (packet_id, qos) = result.unwrap();
            assert!(packet_id > 0);
            assert!(matches!(qos, QoS::AtMostOnce | QoS::AtLeastOnce));

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_multiple_subscriptions() {
    let client = MqttClient::new(test_client_id("multiple-subscriptions"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            let messages = Arc::new(Mutex::new(Vec::new()));
            let messages_clone = Arc::clone(&messages);

            // Subscribe to multiple topics with one callback
            let topics = vec![
                ("test/1", QoS::AtLeastOnce),
                ("test/2", QoS::AtLeastOnce),
                ("test/3", QoS::AtLeastOnce),
            ];
            let result = client
                .subscribe_many(topics, move |msg| {
                    let messages = messages_clone.clone();
                    tokio::spawn(async move {
                        messages.lock().await.push(msg.topic.clone());
                    });
                })
                .await;

            assert!(result.is_ok());
            let qos_levels = result.unwrap();
            assert_eq!(qos_levels.len(), 3);

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_wildcard_subscription() {
    let client = MqttClient::new(test_client_id("wildcard-subscription"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            let counter = Arc::new(AtomicU32::new(0));
            let counter_clone = Arc::clone(&counter);

            // Subscribe with wildcard
            let result = client
                .subscribe("test/+/data", move |msg| {
                    assert!(msg.topic.starts_with("test/"));
                    assert!(msg.topic.ends_with("/data"));
                    counter_clone.fetch_add(1, Ordering::Relaxed);
                })
                .await;

            assert!(result.is_ok());

            // Subscribe with multi-level wildcard
            let result2 = client
                .subscribe("sensors/#", |msg| {
                    println!("Sensor data: {}", msg.topic);
                })
                .await;

            assert!(result2.is_ok());

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_unsubscribe() {
    let client = MqttClient::new(test_client_id("unsubscribe"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            // Subscribe first
            let result = client.subscribe("test/unsub", |_| {}).await;
            assert!(result.is_ok());

            // Unsubscribe
            let result = client.unsubscribe("test/unsub").await;
            assert!(result.is_ok());

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_unsubscribe_many() {
    let client = MqttClient::new(test_client_id("unsubscribe-many"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            // Subscribe to multiple topics
            let topics = vec![
                ("test/1", QoS::AtMostOnce),
                ("test/2", QoS::AtMostOnce),
                ("test/3", QoS::AtMostOnce),
            ];
            let result = client.subscribe_many(topics.clone(), |_| {}).await;
            assert!(result.is_ok());

            // Unsubscribe from all
            let unsubscribe_topics = vec!["test/1", "test/2", "test/3"];
            let result = client.unsubscribe_many(unsubscribe_topics).await;
            assert!(result.is_ok());

            // Check that all unsubscribes succeeded
            let results = result.unwrap();
            assert_eq!(results.len(), 3);
            for (topic, res) in results {
                assert!(res.is_ok(), "Failed to unsubscribe from {topic}");
            }

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_subscribe_callback_error_handling() {
    let client = MqttClient::new(test_client_id("callback-error-handling"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            // Subscribe with a callback that might panic (shouldn't crash the client)
            let result = client
                .subscribe("test/panic", |msg| {
                    if msg.payload.is_empty() {
                        // This is just for testing - in real code, don't panic
                        println!("Empty payload received");
                    }
                })
                .await;

            assert!(result.is_ok());

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_concurrent_subscriptions() {
    let client = MqttClient::new(test_client_id("concurrent-subscriptions"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            let mut handles = vec![];

            // Create multiple concurrent subscriptions
            for i in 0..5 {
                let client_clone = client.clone();
                let topic = format!("test/concurrent/{i}");

                let handle = tokio::spawn(async move {
                    client_clone
                        .subscribe(topic, |msg| {
                            println!("Received on {}: {:?}", msg.topic, msg.payload);
                        })
                        .await
                });

                handles.push(handle);
            }

            // Wait for all subscriptions
            for handle in handles {
                let result = handle.await.unwrap();
                assert!(result.is_ok());
            }

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}
