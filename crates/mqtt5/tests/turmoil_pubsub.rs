//! Comprehensive pub/sub tests using Turmoil
//!
//! These tests verify publish/subscribe functionality in a deterministic
//! environment, testing various `QoS` levels, topic patterns, and edge cases.

#[cfg(feature = "turmoil-testing")]
use mqtt5::broker::router::MessageRouter;
#[cfg(feature = "turmoil-testing")]
use mqtt5::packet::publish::PublishPacket;
#[cfg(feature = "turmoil-testing")]
use mqtt5::time::Duration;
#[cfg(feature = "turmoil-testing")]
use mqtt5::types::ProtocolVersion;
#[cfg(feature = "turmoil-testing")]
use mqtt5::QoS;
#[cfg(feature = "turmoil-testing")]
use std::sync::Arc;
#[cfg(feature = "turmoil-testing")]
use tokio::sync::mpsc;

#[cfg(feature = "turmoil-testing")]
#[test]
fn test_basic_publish_subscribe() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(10))
        .build();

    sim.host("pubsub-basic", || async {
        let router = Arc::new(MessageRouter::new());

        // Create publisher and subscriber
        let (sub_tx, mut sub_rx) = mpsc::channel(100);

        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client("subscriber".to_string(), sub_tx, dtx)
            .await;
        router
            .subscribe(
                "subscriber".to_string(),
                "test/topic".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Publish a message
        let publish =
            PublishPacket::new("test/topic".to_string(), b"Hello, World!", QoS::AtMostOnce);
        router.route_message(&publish, None).await;

        // Give time for message routing
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify message received
        let msg = sub_rx.try_recv().expect("Should receive message");
        assert_eq!(msg.payload, b"Hello, World!");
        assert_eq!(msg.topic_name, "test/topic");
        assert_eq!(msg.qos, QoS::AtMostOnce);

        // No more messages
        assert!(sub_rx.try_recv().is_err());

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    sim.run().unwrap();
}

#[cfg(feature = "turmoil-testing")]
#[test]
fn test_wildcard_subscriptions() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(15))
        .build();

    sim.host("wildcard-test", || async {
        let router = Arc::new(MessageRouter::new());

        // Create subscribers with different wildcard patterns
        let (single_tx, mut single_rx) = mpsc::channel(100);
        let (multi_tx, mut multi_rx) = mpsc::channel(100);

        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        router
            .register_client("single_wildcard".to_string(), single_tx, dtx1)
            .await;
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client("multi_wildcard".to_string(), multi_tx, dtx2)
            .await;

        // Single-level wildcard subscription
        router
            .subscribe(
                "single_wildcard".to_string(),
                "sensors/+/temperature".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Multi-level wildcard subscription
        router
            .subscribe(
                "multi_wildcard".to_string(),
                "sensors/#".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Publish messages to various topics
        let topics_and_payloads = vec![
            ("sensors/room1/temperature", "22.5"),
            ("sensors/room2/temperature", "23.1"),
            ("sensors/room1/humidity", "45"),
            ("sensors/kitchen/temperature", "24.0"),
            ("sensors/outdoor/weather/temperature", "15.2"),
            ("alerts/critical", "System failure"),
        ];

        for (topic, payload) in &topics_and_payloads {
            let publish =
                PublishPacket::new((*topic).to_string(), payload.as_bytes(), QoS::AtMostOnce);
            router.route_message(&publish, None).await;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Count messages received by single wildcard subscriber
        let mut single_count = 0;
        let mut single_messages = Vec::new();
        while let Ok(msg) = single_rx.try_recv() {
            single_count += 1;
            single_messages.push(msg.topic_name);
        }

        // Count messages received by multi wildcard subscriber
        let mut multi_count = 0;
        let mut multi_messages = Vec::new();
        while let Ok(msg) = multi_rx.try_recv() {
            multi_count += 1;
            multi_messages.push(msg.topic_name);
        }

        // Single wildcard should get 3 temperature messages
        assert_eq!(single_count, 3);
        assert!(single_messages.contains(&"sensors/room1/temperature".to_string()));
        assert!(single_messages.contains(&"sensors/room2/temperature".to_string()));
        assert!(single_messages.contains(&"sensors/kitchen/temperature".to_string()));

        // Multi wildcard should get 5 sensor messages (all under sensors/*)
        assert_eq!(multi_count, 5);
        assert!(multi_messages.contains(&"sensors/room1/temperature".to_string()));
        assert!(multi_messages.contains(&"sensors/room2/temperature".to_string()));
        assert!(multi_messages.contains(&"sensors/room1/humidity".to_string()));
        assert!(multi_messages.contains(&"sensors/kitchen/temperature".to_string()));
        assert!(multi_messages.contains(&"sensors/outdoor/weather/temperature".to_string()));

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    sim.run().unwrap();
}

#[cfg(feature = "turmoil-testing")]
#[test]
fn test_multiple_subscribers_same_topic() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(12))
        .build();

    sim.host("multi-sub-test", || async {
        let router = Arc::new(MessageRouter::new());

        // Create multiple subscribers to the same topic
        let (sub1_tx, mut sub1_rx) = mpsc::channel(100);
        let (sub2_tx, mut sub2_rx) = mpsc::channel(100);
        let (sub3_tx, mut sub3_rx) = mpsc::channel(100);

        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        router
            .register_client("subscriber1".to_string(), sub1_tx, dtx1)
            .await;
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client("subscriber2".to_string(), sub2_tx, dtx2)
            .await;
        let (dtx3, _drx3) = tokio::sync::oneshot::channel();
        router
            .register_client("subscriber3".to_string(), sub3_tx, dtx3)
            .await;

        // All subscribe to the same topic
        let topic = "broadcast/announcement";
        for client in ["subscriber1", "subscriber2", "subscriber3"] {
            router
                .subscribe(
                    client.to_string(),
                    topic.to_string(),
                    QoS::AtMostOnce,
                    None,
                    false,
                    false,
                    0,
                    ProtocolVersion::V5,
                )
                .await
                .unwrap();
        }

        // Publish multiple messages
        for i in 0..5 {
            let publish = PublishPacket::new(
                topic.to_string(),
                format!("Message {i}").as_bytes(),
                QoS::AtMostOnce,
            );
            router.route_message(&publish, None).await;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Each subscriber should receive all messages
        for (name, rx) in [
            ("subscriber1", &mut sub1_rx),
            ("subscriber2", &mut sub2_rx),
            ("subscriber3", &mut sub3_rx),
        ] {
            let mut count = 0;
            let mut messages = Vec::new();
            while let Ok(msg) = rx.try_recv() {
                count += 1;
                messages.push(String::from_utf8(msg.payload).unwrap());
            }

            assert_eq!(count, 5, "{name} should receive 5 messages");
            for i in 0..5 {
                assert!(messages.contains(&format!("Message {i}")));
            }
        }

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    sim.run().unwrap();
}

#[cfg(feature = "turmoil-testing")]
#[test]
fn test_qos_levels() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(10))
        .build();

    sim.host("qos-test", || async {
        let router = Arc::new(MessageRouter::new());

        // Create subscribers for different QoS levels
        let (qos0_tx, mut qos0_rx) = mpsc::channel(100);
        let (qos1_tx, mut qos1_rx) = mpsc::channel(100);

        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        router
            .register_client("qos0_client".to_string(), qos0_tx, dtx1)
            .await;
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client("qos1_client".to_string(), qos1_tx, dtx2)
            .await;

        // Subscribe with different QoS levels
        router
            .subscribe(
                "qos0_client".to_string(),
                "data/qos0".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        router
            .subscribe(
                "qos1_client".to_string(),
                "data/qos1".to_string(),
                QoS::AtLeastOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Publish with different QoS levels
        let qos0_msg =
            PublishPacket::new("data/qos0".to_string(), b"QoS 0 message", QoS::AtMostOnce);
        router.route_message(&qos0_msg, None).await;

        let qos1_msg =
            PublishPacket::new("data/qos1".to_string(), b"QoS 1 message", QoS::AtLeastOnce);
        router.route_message(&qos1_msg, None).await;

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verify QoS 0 message
        let msg0 = qos0_rx.try_recv().expect("Should receive QoS 0 message");
        assert_eq!(msg0.payload, b"QoS 0 message");
        assert_eq!(msg0.qos, QoS::AtMostOnce);

        // Verify QoS 1 message
        let msg1 = qos1_rx.try_recv().expect("Should receive QoS 1 message");
        assert_eq!(msg1.payload, b"QoS 1 message");
        assert_eq!(msg1.qos, QoS::AtLeastOnce);

        // No additional messages
        assert!(qos0_rx.try_recv().is_err());
        assert!(qos1_rx.try_recv().is_err());

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    sim.run().unwrap();
}

#[cfg(feature = "turmoil-testing")]
#[test]
fn test_unsubscribe_functionality() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(15))
        .build();

    sim.host("unsubscribe-test", || async {
        let router = Arc::new(MessageRouter::new());

        let (sub_tx, mut sub_rx) = mpsc::channel(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client("test_client".to_string(), sub_tx, dtx)
            .await;

        // Subscribe to topic
        router
            .subscribe(
                "test_client".to_string(),
                "test/unsubscribe".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Publish first message
        let msg1 = PublishPacket::new(
            "test/unsubscribe".to_string(),
            b"Before unsubscribe",
            QoS::AtMostOnce,
        );
        router.route_message(&msg1, None).await;

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should receive the message
        let received = sub_rx
            .try_recv()
            .expect("Should receive message before unsubscribe");
        assert_eq!(received.payload, b"Before unsubscribe");

        // Unsubscribe
        router.unsubscribe("test_client", "test/unsubscribe").await;

        // Publish second message
        let msg2 = PublishPacket::new(
            "test/unsubscribe".to_string(),
            b"After unsubscribe",
            QoS::AtMostOnce,
        );
        router.route_message(&msg2, None).await;

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should not receive the second message
        assert!(
            sub_rx.try_recv().is_err(),
            "Should not receive message after unsubscribe"
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    sim.run().unwrap();
}
