#![allow(clippy::large_futures)]

mod common;
use common::TestBroker;

use mqtt5::time::Duration;
use mqtt5::{MqttClient, QoS, SubscribeOptions};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::time::sleep;

#[tokio::test]
async fn test_basic_pub_sub_separation() {
    // Start test broker
    let broker = TestBroker::start().await;

    let publisher = MqttClient::new("test-publisher");
    let subscriber = MqttClient::new("test-subscriber");

    // Connect both clients
    publisher.connect(broker.address()).await.unwrap();
    subscriber.connect(broker.address()).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    // Subscribe first
    subscriber
        .subscribe("test/separation/basic", move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    // Give subscription time to establish
    sleep(Duration::from_millis(100)).await;

    // Publish from separate client
    publisher
        .publish("test/separation/basic", "Hello from publisher")
        .await
        .unwrap();

    // Wait for message
    sleep(Duration::from_millis(500)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        1,
        "Should receive one message"
    );

    publisher.disconnect().await.unwrap();
    subscriber.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_multiple_publishers_one_subscriber() {
    // Start test broker
    let broker = TestBroker::start().await;

    let pub1 = MqttClient::new("multi-pub-1");
    let pub2 = MqttClient::new("multi-pub-2");
    let pub3 = MqttClient::new("multi-pub-3");
    let subscriber = MqttClient::new("multi-sub");

    // Connect all clients
    pub1.connect(broker.address()).await.unwrap();
    pub2.connect(broker.address()).await.unwrap();
    pub3.connect(broker.address()).await.unwrap();
    subscriber.connect(broker.address()).await.unwrap();

    let messages = Arc::new(std::sync::Mutex::new(Vec::new()));
    let messages_clone = messages.clone();

    // Subscribe to receive from all publishers
    subscriber
        .subscribe("test/multi/pub", move |msg| {
            messages_clone
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&msg.payload).to_string());
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(100)).await;

    // Publish from different clients
    pub1.publish("test/multi/pub", "Message from pub1")
        .await
        .unwrap();
    pub2.publish("test/multi/pub", "Message from pub2")
        .await
        .unwrap();
    pub3.publish("test/multi/pub", "Message from pub3")
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    {
        let msgs = messages.lock().unwrap();
        assert_eq!(msgs.len(), 3, "Should receive all 3 messages");
        assert!(msgs.contains(&"Message from pub1".to_string()));
        assert!(msgs.contains(&"Message from pub2".to_string()));
        assert!(msgs.contains(&"Message from pub3".to_string()));
    } // Drop the lock before awaiting

    pub1.disconnect().await.unwrap();
    pub2.disconnect().await.unwrap();
    pub3.disconnect().await.unwrap();
    subscriber.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_one_publisher_multiple_subscribers() {
    // Start test broker
    let broker = TestBroker::start().await;

    let publisher = MqttClient::new("one-pub");
    let sub1 = MqttClient::new("multi-sub-1");
    let sub2 = MqttClient::new("multi-sub-2");
    let sub3 = MqttClient::new("multi-sub-3");

    // Connect all
    publisher.connect(broker.address()).await.unwrap();
    sub1.connect(broker.address()).await.unwrap();
    sub2.connect(broker.address()).await.unwrap();
    sub3.connect(broker.address()).await.unwrap();

    let count1 = Arc::new(AtomicU32::new(0));
    let count2 = Arc::new(AtomicU32::new(0));
    let count3 = Arc::new(AtomicU32::new(0));

    let count1_clone = count1.clone();
    let count2_clone = count2.clone();
    let count3_clone = count3.clone();

    // All subscribe to same topic
    sub1.subscribe("test/one/to/many", move |_| {
        count1_clone.fetch_add(1, Ordering::Relaxed);
    })
    .await
    .unwrap();

    sub2.subscribe("test/one/to/many", move |_| {
        count2_clone.fetch_add(1, Ordering::Relaxed);
    })
    .await
    .unwrap();

    sub3.subscribe("test/one/to/many", move |_| {
        count3_clone.fetch_add(1, Ordering::Relaxed);
    })
    .await
    .unwrap();

    sleep(Duration::from_millis(200)).await;

    // Publish once
    publisher
        .publish("test/one/to/many", "Broadcast message")
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    // All subscribers should receive the message
    assert_eq!(count1.load(Ordering::Relaxed), 1);
    assert_eq!(count2.load(Ordering::Relaxed), 1);
    assert_eq!(count3.load(Ordering::Relaxed), 1);

    publisher.disconnect().await.unwrap();
    sub1.disconnect().await.unwrap();
    sub2.disconnect().await.unwrap();
    sub3.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_qos_levels_separate_clients() {
    // Start test broker
    let broker = TestBroker::start().await;

    let publisher = MqttClient::new("qos-pub");
    let subscriber = MqttClient::new("qos-sub");

    publisher.connect(broker.address()).await.unwrap();
    subscriber.connect(broker.address()).await.unwrap();

    let qos_messages = Arc::new(std::sync::Mutex::new(Vec::new()));
    let qos_messages_clone = qos_messages.clone();

    // Subscribe with QoS 2
    subscriber
        .subscribe_with_options(
            "test/qos/separate",
            SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            move |msg| {
                qos_messages_clone
                    .lock()
                    .unwrap()
                    .push((String::from_utf8_lossy(&msg.payload).to_string(), msg.qos));
            },
        )
        .await
        .unwrap();

    sleep(Duration::from_millis(100)).await;

    // Publish with different QoS levels
    publisher
        .publish_qos0("test/qos/separate", "QoS 0 message")
        .await
        .unwrap();
    publisher
        .publish_qos1("test/qos/separate", "QoS 1 message")
        .await
        .unwrap();
    publisher
        .publish_qos2("test/qos/separate", "QoS 2 message")
        .await
        .unwrap();

    sleep(Duration::from_millis(1000)).await;

    {
        let messages = qos_messages.lock().unwrap();
        assert_eq!(messages.len(), 3);

        // Check QoS levels (may be downgraded based on subscription)
        for (payload, qos) in messages.iter() {
            println!("Received: {payload} with QoS {qos:?}");
        }
    } // Drop the lock before awaiting

    publisher.disconnect().await.unwrap();
    subscriber.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_wildcard_subscriptions_separate_clients() {
    // Start test broker
    let broker = TestBroker::start().await;

    let pub1 = MqttClient::new("wild-pub-1");
    let pub2 = MqttClient::new("wild-pub-2");
    let subscriber = MqttClient::new("wild-sub");

    pub1.connect(broker.address()).await.unwrap();
    pub2.connect(broker.address()).await.unwrap();
    subscriber.connect(broker.address()).await.unwrap();

    let topics_received = Arc::new(std::sync::Mutex::new(Vec::new()));
    let topics_clone = topics_received.clone();

    // Subscribe with wildcards
    subscriber
        .subscribe("test/wild/+/data", move |msg| {
            topics_clone.lock().unwrap().push(msg.topic.clone());
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(100)).await;

    // Publish to different matching topics
    pub1.publish("test/wild/sensor1/data", "Data from sensor 1")
        .await
        .unwrap();
    pub2.publish("test/wild/sensor2/data", "Data from sensor 2")
        .await
        .unwrap();
    pub1.publish("test/wild/device/data", "Data from device")
        .await
        .unwrap();

    // This shouldn't match
    pub2.publish("test/wild/data", "No sublevel").await.unwrap();

    sleep(Duration::from_millis(500)).await;

    {
        let topics = topics_received.lock().unwrap();
        assert_eq!(
            topics.len(),
            3,
            "Should receive 3 messages matching the pattern"
        );
        assert!(topics.contains(&"test/wild/sensor1/data".to_string()));
        assert!(topics.contains(&"test/wild/sensor2/data".to_string()));
        assert!(topics.contains(&"test/wild/device/data".to_string()));
    } // Drop the lock before awaiting

    pub1.disconnect().await.unwrap();
    pub2.disconnect().await.unwrap();
    subscriber.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_retained_messages_separate_clients() {
    // Start test broker
    let broker = TestBroker::start().await;

    let publisher = MqttClient::new("retain-pub");

    // Publish retained message and disconnect
    publisher.connect(broker.address()).await.unwrap();
    publisher
        .publish_retain("test/retained/separate", "Retained message")
        .await
        .unwrap();
    publisher.disconnect().await.unwrap();

    // Wait a bit
    sleep(Duration::from_millis(500)).await;

    // New subscriber should receive retained message
    let subscriber = MqttClient::new("retain-sub");
    subscriber.connect(broker.address()).await.unwrap();

    let retained_received = Arc::new(AtomicU32::new(0));
    let retained_clone = retained_received.clone();

    subscriber
        .subscribe("test/retained/separate", move |msg| {
            assert!(msg.retain, "Should have retain flag set");
            retained_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    // Wait for retained message
    sleep(Duration::from_millis(500)).await;

    assert_eq!(
        retained_received.load(Ordering::Relaxed),
        1,
        "Should receive retained message"
    );

    // Clear retained message
    subscriber
        .publish_retain("test/retained/separate", "")
        .await
        .unwrap();

    subscriber.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_concurrent_publishers() {
    // Start test broker
    let broker = TestBroker::start().await;

    let subscriber = MqttClient::new("concurrent-sub");
    subscriber.connect(broker.address()).await.unwrap();

    let counter = Arc::new(AtomicU32::new(0));
    let counter_clone = counter.clone();

    subscriber
        .subscribe("test/concurrent/+", move |_| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(100)).await;

    // Spawn multiple publishers concurrently
    let mut handles = Vec::new();
    let broker_addr = broker.address().to_string();

    for i in 0..10 {
        let addr = broker_addr.clone();
        let handle = tokio::spawn(async move {
            let publisher = MqttClient::new(format!("concurrent-pub-{i}"));
            publisher.connect(&addr).await.unwrap();

            // Each publisher sends 10 messages
            for j in 0..10 {
                publisher
                    .publish(
                        format!("test/concurrent/{i}"),
                        format!("Message {j} from publisher {i}"),
                    )
                    .await
                    .unwrap();
            }

            publisher.disconnect().await.unwrap();
        });
        handles.push(handle);
    }

    // Wait for all publishers to complete
    for handle in handles {
        handle.await.unwrap();
    }

    // Give time for all messages to arrive
    sleep(Duration::from_secs(1)).await;

    let total = counter.load(Ordering::Relaxed);
    println!("Received {total} messages from concurrent publishers");
    assert_eq!(total, 100, "Should receive all 100 messages");

    subscriber.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_publisher_subscriber_isolation() {
    // Start test broker
    let broker = TestBroker::start().await;

    // Test that a client can both publish and subscribe without interference
    let client1 = MqttClient::new("iso-client-1");
    let client2 = MqttClient::new("iso-client-2");

    client1.connect(broker.address()).await.unwrap();
    client2.connect(broker.address()).await.unwrap();

    let client1_received = Arc::new(AtomicU32::new(0));
    let client2_received = Arc::new(AtomicU32::new(0));

    let c1_clone = client1_received.clone();
    let c2_clone = client2_received.clone();

    // Both clients subscribe to each other's topics
    client1
        .subscribe("from/client2", move |_| {
            c1_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    client2
        .subscribe("from/client1", move |_| {
            c2_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(100)).await;

    // Clients publish to each other
    for i in 0..5 {
        client1
            .publish("from/client1", format!("Message {i} from client1"))
            .await
            .unwrap();
        client2
            .publish("from/client2", format!("Message {i} from client2"))
            .await
            .unwrap();
    }

    sleep(Duration::from_millis(500)).await;

    assert_eq!(client1_received.load(Ordering::Relaxed), 5);
    assert_eq!(client2_received.load(Ordering::Relaxed), 5);

    client1.disconnect().await.unwrap();
    client2.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_late_subscriber() {
    // Start test broker
    let broker = TestBroker::start().await;

    let publisher = MqttClient::new("late-pub");
    publisher.connect(broker.address()).await.unwrap();

    // Publish messages before subscriber connects
    for i in 0..5 {
        publisher
            .publish("test/late/sub", format!("Early message {i}"))
            .await
            .unwrap();
    }

    // Wait a bit
    sleep(Duration::from_millis(200)).await;

    // Now connect subscriber
    let subscriber = MqttClient::new("late-sub");
    subscriber.connect(broker.address()).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    subscriber
        .subscribe("test/late/sub", move |_| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    // Publish more messages after subscription
    for i in 5..10 {
        publisher
            .publish("test/late/sub", format!("Late message {i}"))
            .await
            .unwrap();
    }

    sleep(Duration::from_millis(500)).await;

    // Should only receive messages published after subscription
    let count = received.load(Ordering::Relaxed);
    assert_eq!(
        count, 5,
        "Should only receive messages published after subscribing"
    );

    publisher.disconnect().await.unwrap();
    subscriber.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_publish_subscribe_timing() {
    // Start test broker
    let broker = TestBroker::start().await;

    // Test subscribe-first scenario
    {
        let publisher = MqttClient::new("timing-pub-subfirst");
        let subscriber = MqttClient::new("timing-sub-subfirst");

        publisher.connect(broker.address()).await.unwrap();
        subscriber.connect(broker.address()).await.unwrap();

        let received = Arc::new(AtomicU32::new(0));
        let received_clone = received.clone();

        // Subscribe first
        subscriber
            .subscribe("test/timing/subfirst", move |_| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            })
            .await
            .unwrap();

        // Wait for subscription to establish
        sleep(Duration::from_millis(200)).await;

        // Then publish
        publisher
            .publish("test/timing/subfirst", "Test message")
            .await
            .unwrap();

        sleep(Duration::from_millis(500)).await;

        assert_eq!(
            received.load(Ordering::Relaxed),
            1,
            "Subscribe-first should receive the message"
        );

        publisher.disconnect().await.unwrap();
        subscriber.disconnect().await.unwrap();
    }

    // Test publish-first scenario
    {
        let publisher = MqttClient::new("timing-pub-pubfirst");
        let subscriber = MqttClient::new("timing-sub-pubfirst");

        publisher.connect(broker.address()).await.unwrap();
        subscriber.connect(broker.address()).await.unwrap();

        let received = Arc::new(AtomicU32::new(0));
        let received_clone = received.clone();

        // Publish first (before subscription)
        publisher
            .publish("test/timing/pubfirst", "Test message")
            .await
            .unwrap();

        // Wait a bit
        sleep(Duration::from_millis(200)).await;

        // Then subscribe
        subscriber
            .subscribe("test/timing/pubfirst", move |_| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            })
            .await
            .unwrap();

        // Wait to see if message arrives (it shouldn't)
        sleep(Duration::from_millis(500)).await;

        assert_eq!(
            received.load(Ordering::Relaxed),
            0,
            "Publish-first should not receive (no subscription at publish time)"
        );

        publisher.disconnect().await.unwrap();
        subscriber.disconnect().await.unwrap();
    }
}
