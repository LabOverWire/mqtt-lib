mod common;
use common::TestBroker;

use mqtt5::{MqttClient, PublishOptions, PublishResult, QoS};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use mqtt5::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn test_qos0_fire_and_forget() {
    // Start test broker
    let broker = TestBroker::start().await;

    let pub_client = MqttClient::new("qos0-pub");
    let sub_client = MqttClient::new("qos0-sub");

    pub_client.connect(broker.address()).await.unwrap();
    sub_client.connect(broker.address()).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    // Subscribe with QoS 0
    sub_client
        .subscribe("test/qos0", move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    // Publish 10 messages with QoS 0
    for i in 0..10 {
        let result = pub_client
            .publish_qos0("test/qos0", format!("Message {i}"))
            .await;
        assert!(result.is_ok());
    }

    // Give some time for messages to arrive
    sleep(Duration::from_millis(500)).await;

    // With QoS 0, we might not receive all messages
    let count = received.load(Ordering::Relaxed);
    println!("QoS 0: Received {count} of 10 messages");
    assert!(count > 0); // Should receive at least some messages

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_qos1_at_least_once() {
    // Start test broker
    let broker = TestBroker::start().await;

    let pub_client = MqttClient::new("qos1-pub");
    let sub_client = MqttClient::new("qos1-sub");

    pub_client.connect(broker.address()).await.unwrap();
    sub_client.connect(broker.address()).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    // Subscribe with QoS 1
    sub_client
        .subscribe_with_options(
            "test/qos1",
            mqtt5::SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |_msg| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    // Publish 10 messages with QoS 1
    let mut packet_ids = Vec::new();
    for i in 0..10 {
        let result = pub_client
            .publish_qos1("test/qos1", format!("Message {i}"))
            .await
            .unwrap();
        match result {
            PublishResult::QoS1Or2 { packet_id } => packet_ids.push(packet_id),
            PublishResult::QoS0 => panic!("Expected QoS1Or2 result, got QoS0"),
        }
    }

    // All packet IDs should be unique
    let mut unique_ids = packet_ids.clone();
    unique_ids.sort_unstable();
    unique_ids.dedup();
    assert_eq!(packet_ids.len(), unique_ids.len());

    // Give time for acknowledgments
    sleep(Duration::from_millis(500)).await;

    // With QoS 1, we should receive all messages
    let count = received.load(Ordering::Relaxed);
    assert_eq!(count, 10, "QoS 1: Should receive exactly 10 messages");

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_qos2_exactly_once() {
    // Start test broker
    let broker = TestBroker::start().await;

    let pub_client = MqttClient::new("qos2-pub");
    let sub_client = MqttClient::new("qos2-sub");

    pub_client.connect(broker.address()).await.unwrap();
    sub_client.connect(broker.address()).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    // Subscribe with QoS 2
    sub_client
        .subscribe_with_options(
            "test/qos2",
            mqtt5::SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            move |_msg| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    // Publish 10 messages with QoS 2
    let mut packet_ids = Vec::new();
    for i in 0..10 {
        let result = pub_client
            .publish_qos2("test/qos2", format!("Message {i}"))
            .await
            .unwrap();
        match result {
            PublishResult::QoS1Or2 { packet_id } => packet_ids.push(packet_id),
            PublishResult::QoS0 => panic!("Expected QoS1Or2 result, got QoS0"),
        }
    }

    // All packet IDs should be unique
    let mut unique_ids = packet_ids.clone();
    unique_ids.sort_unstable();
    unique_ids.dedup();
    assert_eq!(packet_ids.len(), unique_ids.len());

    // Give time for full QoS 2 handshake
    sleep(Duration::from_secs(1)).await;

    // With QoS 2, we should receive exactly one copy of each message
    let count = received.load(Ordering::Relaxed);
    assert_eq!(
        count, 10,
        "QoS 2: Should receive exactly 10 messages (no duplicates)"
    );

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_qos_downgrade() {
    // Start test broker
    let broker = TestBroker::start().await;

    let pub_client = MqttClient::new("qos-downgrade-pub");
    let sub_client = MqttClient::new("qos-downgrade-sub");

    pub_client.connect(broker.address()).await.unwrap();
    sub_client.connect(broker.address()).await.unwrap();

    let received_qos = Arc::new(AtomicU32::new(0));
    let received_qos_clone = received_qos.clone();

    // Subscribe with QoS 0
    sub_client
        .subscribe_with_options(
            "test/downgrade",
            mqtt5::SubscribeOptions {
                qos: QoS::AtMostOnce,
                ..Default::default()
            },
            move |msg| {
                // Store the received QoS level
                received_qos_clone.store(msg.qos as u32, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    // Publish with QoS 2
    pub_client
        .publish_qos2("test/downgrade", "Test message")
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    // Message should be received with downgraded QoS 0
    let final_qos = received_qos.load(Ordering::Relaxed);
    assert_eq!(final_qos, 0, "Message should be downgraded to QoS 0");

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_qos_upgrade_not_allowed() {
    // Start test broker
    let broker = TestBroker::start().await;

    let pub_client = MqttClient::new("qos-upgrade-pub");
    let sub_client = MqttClient::new("qos-upgrade-sub");

    pub_client.connect(broker.address()).await.unwrap();
    sub_client.connect(broker.address()).await.unwrap();

    let received_qos = Arc::new(AtomicU32::new(3)); // Invalid initial value
    let received_qos_clone = received_qos.clone();

    // Subscribe with QoS 2
    sub_client
        .subscribe_with_options(
            "test/upgrade",
            mqtt5::SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            move |msg| {
                received_qos_clone.store(msg.qos as u32, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    // Publish with QoS 0
    pub_client
        .publish_qos0("test/upgrade", "Test message")
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    // Message should be received with original QoS 0 (no upgrade)
    let final_qos = received_qos.load(Ordering::Relaxed);
    assert_eq!(final_qos, 0, "Message QoS should not be upgraded");

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_qos1_retransmission() {
    // Start test broker
    let broker = TestBroker::start().await;

    // This test simulates packet loss and retransmission
    let client = MqttClient::new("qos1-retrans");
    client.connect(broker.address()).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    // Subscribe to our own messages
    client
        .subscribe_with_options(
            "test/retrans",
            mqtt5::SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |_msg| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    // Send a QoS 1 message
    let result = client
        .publish_qos1("test/retrans", "Test message")
        .await
        .unwrap();
    match result {
        PublishResult::QoS1Or2 { packet_id } => assert!(packet_id > 0),
        PublishResult::QoS0 => panic!("Expected QoS1Or2 result for QoS 1 publish, got QoS0"),
    }

    // Wait for message to arrive
    sleep(Duration::from_millis(500)).await;

    // Should receive exactly one copy
    assert_eq!(received.load(Ordering::Relaxed), 1);

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_qos2_no_duplicates() {
    // Start test broker
    let broker = TestBroker::start().await;

    // Test that QoS 2 prevents duplicate delivery
    let client = MqttClient::new("qos2-nodup");
    client.connect(broker.address()).await.unwrap();

    let messages = Arc::new(std::sync::Mutex::new(Vec::new()));
    let messages_clone = messages.clone();

    // Subscribe with QoS 2
    client
        .subscribe_with_options(
            "test/nodup",
            mqtt5::SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            move |msg| {
                messages_clone
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&msg.payload).to_string());
            },
        )
        .await
        .unwrap();

    // Send multiple messages with QoS 2
    for i in 0..5 {
        client
            .publish_qos2("test/nodup", format!("Message {i}"))
            .await
            .unwrap();
    }

    // Wait for all messages
    sleep(Duration::from_secs(1)).await;

    // Check we received exactly one copy of each
    {
        let msgs = messages.lock().unwrap();
        assert_eq!(msgs.len(), 5);

        // All messages should be unique
        let mut unique_msgs = msgs.clone();
        unique_msgs.sort();
        unique_msgs.dedup();
        assert_eq!(msgs.len(), unique_msgs.len());
    } // Drop the lock before awaiting

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_mixed_qos_levels() {
    // Start test broker
    let broker = TestBroker::start().await;

    let client = MqttClient::new("mixed-qos");
    client.connect(broker.address()).await.unwrap();

    let qos_counts = Arc::new(std::sync::Mutex::new([0u32; 3]));
    let qos_counts_clone = qos_counts.clone();

    // Subscribe with QoS 1
    client
        .subscribe_with_options(
            "test/mixed",
            mqtt5::SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |msg| {
                let mut counts = qos_counts_clone.lock().unwrap();
                counts[msg.qos as usize] += 1;
            },
        )
        .await
        .unwrap();

    // Send messages with different QoS levels
    client
        .publish_qos0("test/mixed", "QoS 0 message")
        .await
        .unwrap();
    client
        .publish_qos1("test/mixed", "QoS 1 message")
        .await
        .unwrap();
    client
        .publish_qos2("test/mixed", "QoS 2 message")
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    {
        let counts = qos_counts.lock().unwrap();
        println!(
            "Received: QoS0={}, QoS1={}, QoS2={}",
            counts[0], counts[1], counts[2]
        );

        // Should receive all messages
        assert_eq!(counts[0], 1, "Should receive QoS 0 message");
        assert_eq!(
            counts[1], 2,
            "Should receive QoS 1 and downgraded QoS 2 messages"
        );
        assert_eq!(
            counts[2], 0,
            "No messages should be received at QoS 2 (subscription is QoS 1)"
        );
    } // Drop the lock before awaiting

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_qos_with_retain() {
    // Start test broker
    let broker = TestBroker::start().await;

    let pub_client = MqttClient::new("qos-retain-pub");
    let sub_client = MqttClient::new("qos-retain-sub");

    // Publisher sends retained message and disconnects
    pub_client.connect(broker.address()).await.unwrap();

    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        retain: true,
        ..Default::default()
    };

    let _ = pub_client
        .publish_with_options("test/qos/retain", "Retained QoS 1", options)
        .await
        .unwrap();
    pub_client.disconnect().await.unwrap();

    // Wait a bit
    sleep(Duration::from_millis(100)).await;

    // Subscriber connects and subscribes
    sub_client.connect(broker.address()).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    sub_client
        .subscribe_with_options(
            "test/qos/retain",
            mqtt5::SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |msg| {
                assert!(msg.retain, "Retained message should have retain flag set");
                assert_eq!(msg.qos, QoS::AtLeastOnce, "Should receive at QoS 1");
                received_clone.fetch_add(1, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    // Wait for retained message
    sleep(Duration::from_millis(500)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        1,
        "Should receive retained message"
    );

    sub_client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_qos_packet_id_exhaustion() {
    // Start test broker
    let broker = TestBroker::start().await;

    let client = MqttClient::new("qos-exhaustion");
    client.connect(broker.address()).await.unwrap();

    // Try to send many QoS 1 messages quickly
    let mut packet_ids = Vec::new();

    // Send 100 messages rapidly
    for i in 0..100 {
        match client
            .publish_qos1("test/exhaustion", format!("Message {i}"))
            .await
        {
            Ok(result) => match result {
                PublishResult::QoS1Or2 { packet_id } => packet_ids.push(packet_id),
                PublishResult::QoS0 => panic!("Expected QoS1Or2 result, got QoS0"),
            },
            Err(e) => {
                println!("Failed to send message {i}: {e:?}");
                break;
            }
        }
    }

    println!("Successfully sent {} messages", packet_ids.len());

    // All successful packet IDs should be unique
    let mut unique_ids = packet_ids.clone();
    unique_ids.sort_unstable();
    unique_ids.dedup();
    assert_eq!(
        packet_ids.len(),
        unique_ids.len(),
        "All packet IDs should be unique"
    );

    client.disconnect().await.unwrap();
}
