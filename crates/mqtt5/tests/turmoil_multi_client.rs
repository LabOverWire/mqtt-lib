//! Multi-client interaction tests using Turmoil
//!
//! These tests verify complex interactions between multiple clients
//! in a deterministic environment, focusing on message routing and
//! subscription management.

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
use std::collections::HashMap;
#[cfg(feature = "turmoil-testing")]
use std::sync::Arc;
#[cfg(feature = "turmoil-testing")]
use tokio::sync::mpsc;

#[cfg(feature = "turmoil-testing")]
#[test]
#[allow(clippy::too_many_lines)]
fn test_multi_client_message_routing() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(15))
        .build();

    sim.host("broker-simulation", || async {
        let router = Arc::new(MessageRouter::new());

        // Create multiple clients with different subscription patterns
        let mut clients = HashMap::new();
        let mut receivers = HashMap::new();

        // Client 1: Subscribes to temperature sensors
        let (tx1, rx1) = mpsc::channel(100);
        clients.insert("temp_monitor", tx1);
        receivers.insert("temp_monitor", rx1);
        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "temp_monitor".to_string(),
                clients["temp_monitor"].clone(),
                dtx1,
            )
            .await;
        router
            .subscribe(
                "temp_monitor".to_string(),
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

        // Client 2: Subscribes to humidity sensors
        let (tx2, rx2) = mpsc::channel(100);
        clients.insert("humidity_monitor", tx2);
        receivers.insert("humidity_monitor", rx2);
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "humidity_monitor".to_string(),
                clients["humidity_monitor"].clone(),
                dtx2,
            )
            .await;
        router
            .subscribe(
                "humidity_monitor".to_string(),
                "sensors/+/humidity".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Client 3: Subscribes to all sensors (wildcard)
        let (tx3, rx3) = mpsc::channel(100);
        clients.insert("all_monitor", tx3);
        receivers.insert("all_monitor", rx3);
        let (dtx3, _drx3) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "all_monitor".to_string(),
                clients["all_monitor"].clone(),
                dtx3,
            )
            .await;
        router
            .subscribe(
                "all_monitor".to_string(),
                "sensors/+/+".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Client 4: Subscribes to specific room only
        let (tx4, rx4) = mpsc::channel(100);
        clients.insert("room1_monitor", tx4);
        receivers.insert("room1_monitor", rx4);
        let (dtx4, _drx4) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "room1_monitor".to_string(),
                clients["room1_monitor"].clone(),
                dtx4,
            )
            .await;
        router
            .subscribe(
                "room1_monitor".to_string(),
                "sensors/room1/+".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Publish various sensor messages
        let messages = vec![
            ("sensors/room1/temperature", "22.5"),
            ("sensors/room1/humidity", "45"),
            ("sensors/room2/temperature", "23.1"),
            ("sensors/room2/humidity", "42"),
            ("sensors/kitchen/temperature", "24.0"),
            ("sensors/kitchen/humidity", "50"),
        ];

        for (topic, payload) in &messages {
            let publish =
                PublishPacket::new((*topic).to_string(), payload.as_bytes(), QoS::AtMostOnce);
            router.route_message(&publish, None).await;
        }

        // Give messages time to be routed
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Count messages received by each client
        let mut temp_count = 0;
        let mut humidity_count = 0;
        let mut all_count = 0;
        let mut room1_count = 0;

        // Drain all channels
        while receivers
            .get_mut("temp_monitor")
            .unwrap()
            .try_recv()
            .is_ok()
        {
            temp_count += 1;
        }
        while receivers
            .get_mut("humidity_monitor")
            .unwrap()
            .try_recv()
            .is_ok()
        {
            humidity_count += 1;
        }
        while receivers.get_mut("all_monitor").unwrap().try_recv().is_ok() {
            all_count += 1;
        }
        while receivers
            .get_mut("room1_monitor")
            .unwrap()
            .try_recv()
            .is_ok()
        {
            room1_count += 1;
        }

        // Verify expected message counts
        assert_eq!(temp_count, 3); // 3 temperature messages
        assert_eq!(humidity_count, 3); // 3 humidity messages
        assert_eq!(all_count, 6); // All 6 messages
        assert_eq!(room1_count, 2); // 2 messages from room1

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    sim.run().unwrap();
}

#[cfg(feature = "turmoil-testing")]
#[test]
fn test_client_subscription_changes() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(20))
        .build();

    sim.host("dynamic-subscription", || async {
        let router = Arc::new(MessageRouter::new());

        // Create a client
        let (tx, mut rx) = mpsc::channel(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client("dynamic_client".to_string(), tx, dtx)
            .await;

        // Initial subscription
        router
            .subscribe(
                "dynamic_client".to_string(),
                "alerts/error".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Send error alert
        let error_msg = PublishPacket::new(
            "alerts/error".to_string(),
            b"Critical error occurred",
            QoS::AtMostOnce,
        );
        router.route_message(&error_msg, None).await;

        // Send warning alert (should not be received)
        let warning_msg = PublishPacket::new(
            "alerts/warning".to_string(),
            b"Warning message",
            QoS::AtMostOnce,
        );
        router.route_message(&warning_msg, None).await;

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should have received only the error message
        let mut initial_count = 0;
        while rx.try_recv().is_ok() {
            initial_count += 1;
        }
        assert_eq!(initial_count, 1);

        // Add another subscription for warnings
        router
            .subscribe(
                "dynamic_client".to_string(),
                "alerts/warning".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Send both types of alerts again
        router.route_message(&error_msg, None).await;
        router.route_message(&warning_msg, None).await;

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should now receive both messages
        let mut second_count = 0;
        while rx.try_recv().is_ok() {
            second_count += 1;
        }
        assert_eq!(second_count, 2);

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    sim.run().unwrap();
}

#[cfg(feature = "turmoil-testing")]
#[test]
fn test_message_ordering_with_multiple_clients() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(15))
        .build();

    sim.host("ordering-test", || async {
        let router = Arc::new(MessageRouter::new());

        // Create two clients subscribing to the same topic
        let (tx1, mut rx1) = mpsc::channel(100);
        let (tx2, mut rx2) = mpsc::channel(100);

        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        router
            .register_client("client1".to_string(), tx1, dtx1)
            .await;
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client("client2".to_string(), tx2, dtx2)
            .await;

        router
            .subscribe(
                "client1".to_string(),
                "sequence/test".to_string(),
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
                "client2".to_string(),
                "sequence/test".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Send messages in sequence
        for i in 0..5 {
            let msg = PublishPacket::new(
                "sequence/test".to_string(),
                format!("Message {i}").as_bytes(),
                QoS::AtMostOnce,
            );
            router.route_message(&msg, None).await;

            // Small delay to ensure ordering
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Both clients should receive all messages in order
        let mut client1_messages = Vec::new();
        let mut client2_messages = Vec::new();

        while let Ok(msg) = rx1.try_recv() {
            client1_messages.push(String::from_utf8(msg.payload).unwrap());
        }

        while let Ok(msg) = rx2.try_recv() {
            client2_messages.push(String::from_utf8(msg.payload).unwrap());
        }

        assert_eq!(client1_messages.len(), 5);
        assert_eq!(client2_messages.len(), 5);

        // Verify ordering
        for i in 0..5 {
            assert_eq!(client1_messages[i], format!("Message {i}"));
            assert_eq!(client2_messages[i], format!("Message {i}"));
        }

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    sim.run().unwrap();
}
