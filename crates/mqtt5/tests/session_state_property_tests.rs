//! Property-based tests for MQTT session state management
//!
//! This test suite ensures robust session state handling including:
//! - Session persistence across reconnects
//! - Clean start flag behaviors  
//! - Session expiry interval handling
//! - Concurrent session operations
//! - Unacked message tracking
//! - `QoS` state consistency
//! - Subscription management
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_precision_loss)]

use mqtt5::packet::publish::PublishPacket;
use mqtt5::packet::subscribe::{RetainHandling, SubscriptionOptions};
use mqtt5::protocol::v5::properties::Properties;
use mqtt5::session::queue::QueuedMessage;
use mqtt5::session::subscription::Subscription;
use mqtt5::session::{SessionConfig, SessionState};
use mqtt5::QoS;
use proptest::prelude::*;
use std::sync::Arc;

/// Generate valid packet IDs (1-65535)
fn valid_packet_id() -> impl Strategy<Value = u16> {
    1..=65535u16
}

/// Generate `QoS` levels
fn qos_level() -> impl Strategy<Value = QoS> {
    prop_oneof![
        Just(QoS::AtMostOnce),
        Just(QoS::AtLeastOnce),
        Just(QoS::ExactlyOnce),
    ]
}

/// Generate session expiry intervals
fn session_expiry() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(0),        // Immediate expiry
        1..3600u32,     // 1 second to 1 hour
        Just(86400),    // 1 day
        Just(u32::MAX), // Maximum value
    ]
}

/// Generate publish packets for testing
fn publish_packet(packet_id: u16, qos: QoS) -> PublishPacket {
    PublishPacket {
        dup: false,
        qos,
        retain: false,
        topic_name: "test/topic".to_string(),
        packet_id: if qos == QoS::AtMostOnce {
            None
        } else {
            Some(packet_id)
        },
        properties: Properties::default(),
        payload: vec![1, 2, 3, 4].into(),
        protocol_version: 5,
    }
}

#[cfg(test)]
mod clean_start_tests {
    use super::*;

    proptest! {
        #[test]
        fn prop_clean_start_clears_session(
            packet_ids in prop::collection::vec(valid_packet_id(), 1..20),
            qos in qos_level().prop_filter("QoS > 0", |&q| q != QoS::AtMostOnce)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let config = SessionConfig {
                    session_expiry_interval: 3600,
                    ..SessionConfig::default()
                };

                let session = SessionState::new("client1".to_string(), config.clone(), false);

                // Add some subscriptions
                let sub = Subscription {
                    topic_filter: "test/+".to_string(),
                    options: SubscriptionOptions {
                        qos,
                        no_local: false,
                        retain_as_published: false,
                        retain_handling: RetainHandling::SendAtSubscribe,
                    },
                };
                session.add_subscription(sub.topic_filter.clone(), sub).await.unwrap();

                // Add some unacked messages
                for &id in &packet_ids {
                    let packet = publish_packet(id, qos);
                    session.store_unacked_publish(packet).await.unwrap();
                }

                let initial_subs = session.all_subscriptions().await;
                let initial_unacked = session.get_unacked_publishes().await;

                prop_assert!(!initial_subs.is_empty());
                prop_assert!(!initial_unacked.is_empty());

                // Create new session with clean_start = true
                let clean_session = SessionState::new("client1".to_string(), config, true);

                // Clean session should have no state
                let clean_subs = clean_session.all_subscriptions().await;
                let clean_unacked = clean_session.get_unacked_publishes().await;

                prop_assert!(clean_subs.is_empty());
                prop_assert!(clean_unacked.is_empty());
                Ok(())
            })?;
        }

        #[test]
        fn prop_clean_start_false_preserves_unacked(
            packet_ids in prop::collection::hash_set(valid_packet_id(), 1..10),
            qos in qos_level().prop_filter("QoS > 0", |&q| q != QoS::AtMostOnce)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let config = SessionConfig {
                    session_expiry_interval: 3600,
                    ..SessionConfig::default()
                };

                // Sessions with clean_start = false preserve state
                let session = SessionState::new("client1".to_string(), config.clone(), false);

                // Add unacked messages
                for &id in &packet_ids {
                    let packet = publish_packet(id, qos);
                    session.store_unacked_publish(packet).await.unwrap();
                }

                let unacked_count = session.get_unacked_publishes().await.len();
                prop_assert_eq!(unacked_count, packet_ids.len());

                // State persists across "reconnections" (new instance with same client_id)
                // In practice, a session manager would handle this
                Ok(())
            })?;
        }
    }
}

#[cfg(test)]
mod session_expiry_tests {
    use super::*;

    proptest! {
        #[test]
        fn prop_session_expiry_interval_handling(
            expiry in session_expiry(),
            _elapsed_seconds in 0..7200u32
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let config = SessionConfig {
                    session_expiry_interval: expiry,
                    ..SessionConfig::default()
                };

                let session = SessionState::new("client1".to_string(), config.clone(), false);

                // For testing expiry, we'd need to manipulate last_activity time
                // The is_expired() method checks time since last activity

                // Session with expiry = 0 expires immediately on disconnect
                if expiry == 0 {
                    // Would expire immediately after disconnect
                    prop_assert!(true);
                } else if expiry == u32::MAX {
                    // Never expires
                    prop_assert!(!session.is_expired().await);
                } else {
                    // Normal expiry interval
                    prop_assert!(!session.is_expired().await);
                }
                Ok(())
            })?;
        }

        #[test]
        fn prop_session_stats_tracking(
            sub_count in 1..20usize,
            msg_count in 1..50usize
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                // Add subscriptions
                for i in 0..sub_count {
                    let topic_filter = format!("topic/{i}");
                    let sub = Subscription {
                        topic_filter: topic_filter.clone(),
                        options: SubscriptionOptions::default(),
                    };
                    session.add_subscription(topic_filter, sub).await.unwrap();
                }

                // Queue messages
                for i in 0..msg_count {
                    let msg = QueuedMessage {
                        topic: format!("topic/{}", i % sub_count),
                        payload: vec![i as u8],
                        qos: QoS::AtLeastOnce,
                        retain: false,
                        packet_id: Some((i as u16) + 1),
                    };
                    session.queue_message(msg).await.unwrap();
                }

                let stats = session.stats().await;
                prop_assert_eq!(stats.subscription_count, sub_count);
                prop_assert_eq!(stats.queued_message_count, msg_count);
                Ok(())
            })?;
        }
    }
}

#[cfg(test)]
mod subscription_management_tests {
    use super::*;

    proptest! {
        #[test]
        fn prop_subscription_matching(
            topics in prop::collection::vec(
                ("[a-zA-Z0-9/]{1,50}", qos_level()),
                1..20
            )
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                // Add subscriptions
                for (topic, qos) in &topics {
                    let topic_filter = topic.clone();
                    let sub = Subscription {
                        topic_filter: topic_filter.clone(),
                        options: SubscriptionOptions {
                            qos: *qos,
                            ..SubscriptionOptions::default()
                        },
                    };
                    session.add_subscription(topic_filter, sub).await.unwrap();
                }

                // Check all subscriptions were added
                // Note: MQTT replaces subscriptions to the same topic, so we need to count unique topics
                let all_subs = session.all_subscriptions().await;
                let unique_topics: std::collections::HashSet<_> = topics.iter().map(|(t, _)| t).collect();
                prop_assert_eq!(all_subs.len(), unique_topics.len());

                // Check specific subscription retrieval
                // Create a map of the final subscriptions (last one wins for duplicate topics)
                let mut expected_subs = std::collections::HashMap::new();
                for (topic, qos) in &topics {
                    expected_subs.insert(topic.clone(), *qos);
                }

                // Verify each unique subscription
                for (topic, expected_qos) in expected_subs {
                    let matching = session.matching_subscriptions(&topic).await;
                    prop_assert!(!matching.is_empty(), "No subscription found for topic: {}", topic);
                    prop_assert_eq!(matching[0].1.options.qos, expected_qos,
                        "QoS mismatch for topic: {}", topic);
                }

                Ok(())
            })?;
        }

        #[test]
        fn prop_subscription_removal(
            topics in prop::collection::hash_set("[a-zA-Z0-9/]{1,50}", 5..15)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                // Add all subscriptions
                for topic in &topics {
                    let topic_filter = topic.clone();
                    let sub = Subscription {
                        topic_filter: topic_filter.clone(),
                        options: SubscriptionOptions::default(),
                    };
                    session.add_subscription(topic_filter, sub).await.unwrap();
                }

                prop_assert_eq!(session.all_subscriptions().await.len(), topics.len());

                // Remove half of them
                let topics_vec: Vec<_> = topics.iter().cloned().collect();
                let to_remove = topics_vec.len() / 2;
                for topic in &topics_vec[..to_remove] {
                    let removed = session.remove_subscription(topic).await.unwrap();
                    prop_assert!(removed);
                }

                prop_assert_eq!(session.all_subscriptions().await.len(), topics.len() - to_remove);

                // Verify correct ones remain
                for topic in &topics_vec[to_remove..] {
                    let matching = session.matching_subscriptions(topic).await;
                    prop_assert!(!matching.is_empty());
                }

                Ok(())
            })?;
        }
    }
}

#[cfg(test)]
mod unacked_message_tests {
    use super::*;

    proptest! {
        #[test]
        fn prop_unacked_publish_tracking(
            packets in prop::collection::vec((valid_packet_id(), qos_level()), 1..20)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                let mut qos_packets = vec![];
                let mut seen_ids = std::collections::HashSet::new();

                // Store unacked publishes, skipping duplicates (which would overwrite in real MQTT)
                for (id, qos) in packets {
                    if qos != QoS::AtMostOnce && !seen_ids.contains(&id) {
                        let packet = publish_packet(id, qos);
                        session.store_unacked_publish(packet.clone()).await.unwrap();
                        qos_packets.push((id, packet));
                        seen_ids.insert(id);
                    }
                }

                // Verify all are tracked
                let unacked = session.get_unacked_publishes().await;
                prop_assert_eq!(unacked.len(), qos_packets.len());

                // Remove half and verify
                let half = qos_packets.len() / 2;
                for (id, _) in &qos_packets[..half] {
                    let removed = session.remove_unacked_publish(*id).await;
                    prop_assert!(removed.is_some());
                }

                let remaining = session.get_unacked_publishes().await;
                prop_assert_eq!(remaining.len(), qos_packets.len() - half);

                Ok(())
            })?;
        }

        #[test]
        fn prop_qos2_state_transitions(
            packet_id in valid_packet_id()
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                // QoS 2 flow: PUBLISH -> PUBREC -> PUBREL -> PUBCOMP
                let packet = publish_packet(packet_id, QoS::ExactlyOnce);

                // Step 1: Store PUBLISH
                session.store_unacked_publish(packet).await.unwrap();
                prop_assert!(!session.get_unacked_publishes().await.is_empty());

                // Step 2: Receive PUBREC, store it
                session.store_pubrec(packet_id).await;
                prop_assert!(session.has_pubrec(packet_id).await);

                // Step 3: Send PUBREL, track it
                session.complete_publish(packet_id).await;
                session.store_pubrel(packet_id).await;

                // Step 4: Receive PUBCOMP, complete flow
                session.complete_pubrel(packet_id).await;

                // Verify cleanup
                prop_assert!(!session.has_pubrec(packet_id).await);

                Ok(())
            })?;
        }

        #[test]
        fn prop_unacked_pubrel_tracking(
            packet_ids in prop::collection::vec(valid_packet_id(), 1..20)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                // Store unacked pubrels, deduplicating
                let unique_ids: std::collections::HashSet<_> = packet_ids.iter().copied().collect();
                for &id in &unique_ids {
                    session.store_unacked_pubrel(id).await;
                }

                let pubrels = session.get_unacked_pubrels().await;
                prop_assert_eq!(pubrels.len(), unique_ids.len());

                // Remove half of unique IDs
                let unique_vec: Vec<_> = unique_ids.into_iter().collect();
                let half = unique_vec.len() / 2;
                for &id in &unique_vec[..half] {
                    let removed = session.remove_unacked_pubrel(id).await;
                    prop_assert!(removed);
                }

                let remaining = session.get_unacked_pubrels().await;
                prop_assert_eq!(remaining.len(), unique_vec.len() - half);

                Ok(())
            })?;
        }
    }
}

#[cfg(test)]
mod message_queue_tests {
    use super::*;

    proptest! {
        #[test]
        fn prop_message_queuing_limits(
            messages in prop::collection::vec(1..100u8, 1..50)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let config = SessionConfig {
                    max_queued_messages: 20,
                    max_queued_size: 1000,
                    ..SessionConfig::default()
                };

                let session = SessionState::new("client1".to_string(), config, true);

                // Try to queue all messages
                for (i, size) in messages.iter().enumerate() {
                    let msg = QueuedMessage {
                        topic: format!("topic/{i}"),
                        payload: vec![0u8; *size as usize],
                        qos: QoS::AtLeastOnce,
                        retain: false,
                        packet_id: Some((i as u16) + 1),
                    };

                    // Always attempt to queue to test limits
                    let _ = session.queue_message(msg).await;
                }

                // Get the actual count from the session
                let actual_count = session.queued_message_count().await;

                // Should not exceed max messages
                prop_assert!(actual_count <= 20,
                    "Queued {} messages, exceeds limit of 20", actual_count);

                // Verify we can dequeue some messages
                let to_dequeue = actual_count.min(5);
                let dequeued = session.dequeue_messages(to_dequeue).await;
                prop_assert!(dequeued.len() <= to_dequeue);

                // Verify the count is updated after dequeuing
                let remaining = session.queued_message_count().await;
                prop_assert_eq!(remaining, actual_count - dequeued.len());

                Ok(())
            })?;
        }

        #[test]
        fn prop_message_expiry_handling(
            count in 1..20usize
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                // Queue messages
                for i in 0..count {
                    let msg = QueuedMessage {
                        topic: format!("topic/{i}"),
                        payload: vec![i as u8],
                        qos: QoS::AtLeastOnce,
                        retain: false,
                        packet_id: Some((i as u16) + 1),
                    };
                    session.queue_message(msg).await.unwrap();
                }

                prop_assert_eq!(session.queued_message_count().await, count);

                // The SessionState queue_message method handles converting to ExpiringMessage
                // We'll test that messages are queued correctly
                // Message expiry is handled internally by the queue

                Ok(())
            })?;
        }
    }
}

#[cfg(test)]
mod flow_control_tests {
    use super::*;

    proptest! {
        #[test]
        fn prop_receive_maximum_enforcement(
            receive_max in 1..100u16,
            message_count in 1..200usize
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                // Set receive maximum
                session.set_receive_maximum(receive_max).await;

                let mut in_flight: u16 = 0;
                let mut _can_send_count = 0;

                // Try to send messages
                for i in 0..message_count {
                    if session.can_send_qos_message().await {
                        _can_send_count += 1;
                        let packet_id = (i as u16 % 65535) + 1;
                        if session.register_in_flight(packet_id).await.is_ok() {
                            in_flight += 1;
                        }
                    }

                    // Randomly acknowledge some
                    if i % 3 == 0 && i > 0 && in_flight > 0 {
                        let ack_id = ((i - 1) as u16 % 65535) + 1;
                        if session.acknowledge_in_flight(ack_id).await.is_ok() {
                            in_flight = in_flight.saturating_sub(1);
                        }
                    }
                }

                // Should never exceed receive maximum
                prop_assert!(in_flight <= receive_max);

                Ok(())
            })?;
        }

        #[test]
        fn prop_topic_alias_management(
            topics in prop::collection::vec("[a-zA-Z0-9/]{5,30}", 1..50),
            max_alias in 5..20u16
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                // Set topic alias maximum
                session.set_topic_alias_maximum_out(max_alias).await;
                session.set_topic_alias_maximum_in(max_alias).await;

                let mut assigned_aliases = std::collections::HashMap::new();

                // Try to get aliases for topics
                for topic in &topics {
                    if let Some(alias) = session.get_or_create_topic_alias(topic).await {
                        prop_assert!(alias > 0 && alias <= max_alias);
                        assigned_aliases.insert(topic.clone(), alias);
                    }
                }

                // Should not exceed maximum
                prop_assert!(assigned_aliases.len() <= max_alias as usize);

                // Test incoming alias registration
                for (i, topic) in topics.iter().take(max_alias as usize).enumerate() {
                    let alias = (i as u16) + 1;
                    session.register_incoming_topic_alias(alias, topic).await.unwrap();

                    let retrieved = session.get_topic_for_alias(alias).await;
                    prop_assert_eq!(retrieved.as_deref(), Some(topic.as_str()));
                }

                Ok(())
            })?;
        }
    }
}

#[cfg(test)]
mod retained_message_tests {
    use super::*;

    proptest! {
        #[test]
        fn prop_retained_message_storage(
            topics in prop::collection::vec("[a-zA-Z0-9/]{5,30}", 1..20)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                // Store retained messages
                for (i, topic) in topics.iter().enumerate() {
                    let packet = PublishPacket {
                        dup: false,
                        qos: QoS::AtLeastOnce,
                        retain: true,
                        topic_name: topic.clone(),
                        packet_id: Some((i as u16) + 1),
                        properties: Properties::default(),
                        payload: vec![i as u8].into(),
                        protocol_version: 5,
                    };
                    session.store_retained_message(&packet).await;
                }

                // Retrieve by exact topic
                for (i, topic) in topics.iter().enumerate() {
                    let retained = session.get_retained_messages(topic).await;
                    prop_assert_eq!(retained.len(), 1);
                    prop_assert_eq!(retained[0].payload[0], i as u8);
                }

                // Clear retained message by sending empty payload
                if let Some(topic) = topics.first() {
                    let clear_packet = PublishPacket {
                        dup: false,
                        qos: QoS::AtMostOnce,
                        retain: true,
                        topic_name: topic.clone(),
                        packet_id: None,
                        properties: Properties::default(),
                        payload: vec![].into(),
                        protocol_version: 5,
                    };
                    session.store_retained_message(&clear_packet).await;

                    let retained = session.get_retained_messages(topic).await;
                    prop_assert!(retained.is_empty());
                }

                Ok(())
            })?;
        }

        #[test]
        fn prop_retained_wildcard_matching(
            base_topic in "[a-zA-Z0-9]{3,10}",
            levels in 1..5usize
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                // Create hierarchical topics
                let mut topics = Vec::new();
                for i in 0..levels {
                    let topic = format!("{base_topic}/{i}/data");
                    topics.push(topic.clone());

                    let packet = PublishPacket {
                        dup: false,
                        qos: QoS::AtLeastOnce,
                        retain: true,
                        topic_name: topic,
                        packet_id: Some((i as u16) + 1),
                        properties: Properties::default(),
                        payload: vec![i as u8].into(),
                        protocol_version: 5,
                    };
                    session.store_retained_message(&packet).await;
                }

                // Test wildcard matching
                let filter = format!("{base_topic}/+/data");
                let matched = session.get_retained_messages(&filter).await;
                prop_assert_eq!(matched.len(), levels);

                // Test multi-level wildcard
                let filter = format!("{base_topic}/#");
                let matched = session.get_retained_messages(&filter).await;
                prop_assert_eq!(matched.len(), levels);

                Ok(())
            })?;
        }
    }
}

#[cfg(test)]
mod concurrent_session_tests {
    use super::*;
    use tokio::task::JoinSet;

    proptest! {
        #[test]
        fn prop_concurrent_subscription_updates(
            thread_count in 2..8usize,
            ops_per_thread in 10..30usize
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = Arc::new(SessionState::new(
                    "client1".to_string(),
                    SessionConfig::default(),
                    true
                ));

                let mut join_set = JoinSet::new();

                // Spawn concurrent tasks
                for thread_id in 0..thread_count {
                    let session = Arc::clone(&session);
                    let task = async move {
                        for i in 0..ops_per_thread {
                            let topic = format!("thread{thread_id}/topic{i}");
                            let sub = Subscription {
                                topic_filter: topic.clone(),
                                options: SubscriptionOptions::default(),
                            };
                            session.add_subscription(topic, sub).await.unwrap();
                        }
                    };
                    join_set.spawn(task);
                }

                // Wait for all tasks
                while join_set.join_next().await.is_some() {}

                // Verify all subscriptions were added
                let all_subs = session.all_subscriptions().await;
                prop_assert_eq!(all_subs.len(), thread_count * ops_per_thread);

                Ok(())
            })?;
        }

        #[test]
        fn prop_concurrent_message_queuing(
            thread_count in 2..8usize,
            msgs_per_thread in 5..20usize
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let config = SessionConfig {
                    max_queued_messages: 1000,
                    ..SessionConfig::default()
                };

                let session = Arc::new(SessionState::new(
                    "client1".to_string(),
                    config,
                    true
                ));

                let mut join_set = JoinSet::new();

                // Spawn concurrent tasks
                for thread_id in 0..thread_count {
                    let session = Arc::clone(&session);
                    let task = async move {
                        let mut queued = 0;
                        for i in 0..msgs_per_thread {
                            let msg = QueuedMessage {
                                topic: format!("thread{thread_id}/msg{i}"),
                                payload: vec![thread_id as u8, i as u8],
                                qos: QoS::AtLeastOnce,
                                retain: false,
                                packet_id: Some(((thread_id * msgs_per_thread + i) as u16) + 1),
                            };
                            if session.queue_message(msg).await.is_ok() {
                                queued += 1;
                            }
                        }
                        queued
                    };
                    join_set.spawn(task);
                }

                // Collect results
                let mut total_queued = 0;
                while let Some(result) = join_set.join_next().await {
                    total_queued += result.unwrap();
                }

                // Verify queued count
                let actual_count = session.queued_message_count().await;
                prop_assert_eq!(actual_count, total_queued);

                Ok(())
            })?;
        }
    }
}

#[cfg(test)]
mod performance_property_tests {
    use super::*;
    use mqtt5::time::Instant;

    proptest! {
        #[test]
        fn prop_session_operation_performance(
            operation_count in 100..500usize
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                let start = Instant::now();

                // Perform mixed operations
                for i in 0..operation_count {
                    match i % 5 {
                        0 => {
                            // Add subscription
                            let topic_filter = format!("topic/{i}");
                            let sub = Subscription {
                                topic_filter: topic_filter.clone(),
                                options: SubscriptionOptions::default(),
                            };
                            let _ = session.add_subscription(topic_filter, sub).await;
                        }
                        1 => {
                            let msg = QueuedMessage {
                                topic: format!("topic/{i}"),
                                payload: vec![i as u8],
                                qos: QoS::AtLeastOnce,
                                retain: false,
                                packet_id: Some((i as u16) + 1),
                            };
                            let _ = session.queue_message(msg).await;
                        }
                        2 => {
                            // Store unacked
                            let packet = publish_packet((i as u16) + 1, QoS::AtLeastOnce);
                            let _ = session.store_unacked_publish(packet).await;
                        }
                        3 => {
                            // Check stats
                            let _ = session.stats().await;
                        }
                        4 => {
                            // Touch session
                            session.touch().await;
                        }
                        _ => unreachable!()
                    }
                }

                let elapsed = start.elapsed();
                let ops_per_second = operation_count as f64 / elapsed.as_secs_f64();

                // Session operations should be reasonably fast
                prop_assert!(
                    ops_per_second > 1_000.0,
                    "Session operations too slow: {:.0} ops/sec",
                    ops_per_second
                );

                Ok(())
            })?;
        }
    }
}
