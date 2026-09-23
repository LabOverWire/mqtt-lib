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
    prop_oneof![Just(0), 1..3600u32, Just(86400), Just(u32::MAX),]
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
        stream_id: None,
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

                for &id in &packet_ids {
                    let packet = publish_packet(id, qos);
                    session.store_unacked_publish(packet).await.unwrap();
                }

                let initial_subs = session.all_subscriptions().await;
                let initial_unacked = session.get_unacked_publishes().await;

                prop_assert!(!initial_subs.is_empty());
                prop_assert!(!initial_unacked.is_empty());

                let clean_session = SessionState::new("client1".to_string(), config, true);

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

                let session = SessionState::new("client1".to_string(), config.clone(), false);

                for &id in &packet_ids {
                    let packet = publish_packet(id, qos);
                    session.store_unacked_publish(packet).await.unwrap();
                }

                let unacked_count = session.get_unacked_publishes().await.len();
                prop_assert_eq!(unacked_count, packet_ids.len());

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
            expiry in session_expiry()
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let config = SessionConfig {
                    session_expiry_interval: expiry,
                    ..SessionConfig::default()
                };

                let session = SessionState::new("client1".to_string(), config.clone(), false);


                if expiry != 0 {
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

                for i in 0..sub_count {
                    let topic_filter = format!("topic/{i}");
                    let sub = Subscription {
                        topic_filter: topic_filter.clone(),
                        options: SubscriptionOptions::default(),
                    };
                    session.add_subscription(topic_filter, sub).await.unwrap();
                }

                for i in 0..msg_count {
                    let msg = QueuedMessage {
                        topic: format!("topic/{}", i % sub_count),
                        payload: vec![i.to_le_bytes()[0]],
                        qos: QoS::AtLeastOnce,
                        retain: false,
                        packet_id: Some(u16::try_from(i).unwrap() + 1),
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

                let all_subs = session.all_subscriptions().await;
                let unique_topics: std::collections::HashSet<_> = topics.iter().map(|(t, _)| t).collect();
                prop_assert_eq!(all_subs.len(), unique_topics.len());

                let mut expected_subs = std::collections::HashMap::new();
                for (topic, qos) in &topics {
                    expected_subs.insert(topic.clone(), *qos);
                }

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

                for topic in &topics {
                    let topic_filter = topic.clone();
                    let sub = Subscription {
                        topic_filter: topic_filter.clone(),
                        options: SubscriptionOptions::default(),
                    };
                    session.add_subscription(topic_filter, sub).await.unwrap();
                }

                prop_assert_eq!(session.all_subscriptions().await.len(), topics.len());

                let topics_vec: Vec<_> = topics.iter().cloned().collect();
                let to_remove = topics_vec.len() / 2;
                for topic in &topics_vec[..to_remove] {
                    let removed = session.remove_subscription(topic).await.unwrap();
                    prop_assert!(removed);
                }

                prop_assert_eq!(session.all_subscriptions().await.len(), topics.len() - to_remove);

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

                for (id, qos) in packets {
                    if qos != QoS::AtMostOnce && !seen_ids.contains(&id) {
                        let packet = publish_packet(id, qos);
                        session.store_unacked_publish(packet.clone()).await.unwrap();
                        qos_packets.push((id, packet));
                        seen_ids.insert(id);
                    }
                }

                let unacked = session.get_unacked_publishes().await;
                prop_assert_eq!(unacked.len(), qos_packets.len());

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
        fn prop_qos2_outbound_state_transitions(
            packet_id in valid_packet_id()
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                let packet = publish_packet(packet_id, QoS::ExactlyOnce);

                session.store_unacked_publish(packet).await.unwrap();
                prop_assert!(!session.get_unacked_publishes().await.is_empty());

                session.complete_pubrec(packet_id).await;
                session.store_pubrel(packet_id).await;
                prop_assert!(session.get_unacked_publishes().await.is_empty());
                prop_assert_eq!(session.get_unacked_pubrels().await.len(), 1);

                session.complete_pubrel(packet_id).await;
                prop_assert!(session.get_unacked_pubrels().await.is_empty());

                Ok(())
            })?;
        }

        #[test]
        fn prop_qos2_inbound_state_transitions(
            packet_id in valid_packet_id()
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                prop_assert!(session.mark_pubrec_pending(packet_id).await);
                prop_assert!(session.has_pubrec(packet_id).await);

                prop_assert!(!session.mark_pubrec_pending(packet_id).await);

                session.remove_pubrec(packet_id).await;
                prop_assert!(!session.has_pubrec(packet_id).await);

                Ok(())
            })?;
        }

        #[test]
        fn prop_qos2_directions_do_not_collide(
            packet_id in valid_packet_id()
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let session = SessionState::new("client1".to_string(), SessionConfig::default(), true);

                session.store_pubrel(packet_id).await;

                prop_assert!(
                    session.mark_pubrec_pending(packet_id).await,
                    "an outbound PUBREL must not make an inbound packet id look already-seen"
                );

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

                let unique_ids: std::collections::HashSet<_> = packet_ids.iter().copied().collect();
                for &id in &unique_ids {
                    session.store_unacked_pubrel(id).await;
                }

                let pubrels = session.get_unacked_pubrels().await;
                prop_assert_eq!(pubrels.len(), unique_ids.len());

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

                for (i, size) in messages.iter().enumerate() {
                    let msg = QueuedMessage {
                        topic: format!("topic/{i}"),
                        payload: vec![0u8; *size as usize],
                        qos: QoS::AtLeastOnce,
                        retain: false,
                        packet_id: Some(u16::try_from(i).unwrap() + 1),
                    };

                    let _ = session.queue_message(msg).await;
                }

                let actual_count = session.queued_message_count().await;

                prop_assert!(actual_count <= 20,
                    "Queued {} messages, exceeds limit of 20", actual_count);

                let to_dequeue = actual_count.min(5);
                let dequeued = session.dequeue_messages(to_dequeue).await;
                prop_assert!(dequeued.len() <= to_dequeue);

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

                for i in 0..count {
                    let msg = QueuedMessage {
                        topic: format!("topic/{i}"),
                        payload: vec![i.to_le_bytes()[0]],
                        qos: QoS::AtLeastOnce,
                        retain: false,
                        packet_id: Some(u16::try_from(i).unwrap() + 1),
                    };
                    session.queue_message(msg).await.unwrap();
                }

                prop_assert_eq!(session.queued_message_count().await, count);


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

                session.set_receive_maximum(receive_max).await;

                let mut in_flight: u16 = 0;

                for i in 0..message_count {
                    if session.can_send_qos_message().await {
                        let packet_id = u16::try_from(i).unwrap() + 1;
                        if session.register_in_flight(packet_id).await.is_ok() {
                            in_flight += 1;
                        }
                    }

                    if i % 3 == 0 && i > 0 && in_flight > 0 {
                        let ack_id = u16::try_from(i - 1).unwrap() + 1;
                        if session.acknowledge_in_flight(ack_id).await.is_ok() {
                            in_flight = in_flight.saturating_sub(1);
                        }
                    }
                }

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

                session.set_topic_alias_maximum_out(max_alias).await;
                session.set_topic_alias_maximum_in(max_alias).await;

                let mut assigned_aliases = std::collections::HashMap::new();

                for topic in &topics {
                    if let Some(alias) = session.get_or_create_topic_alias(topic).await {
                        prop_assert!(alias > 0 && alias <= max_alias);
                        assigned_aliases.insert(topic.clone(), alias);
                    }
                }

                prop_assert!(assigned_aliases.len() <= max_alias as usize);

                for (i, topic) in topics.iter().take(max_alias as usize).enumerate() {
                    let alias = u16::try_from(i).unwrap() + 1;
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

                while join_set.join_next().await.is_some() {}

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

                for thread_id in 0..thread_count {
                    let session = Arc::clone(&session);
                    let task = async move {
                        let mut queued = 0;
                        for i in 0..msgs_per_thread {
                            let msg = QueuedMessage {
                                topic: format!("thread{thread_id}/msg{i}"),
                                payload: vec![u8::try_from(thread_id).unwrap(), u8::try_from(i).unwrap()],
                                qos: QoS::AtLeastOnce,
                                retain: false,
                                packet_id: Some(u16::try_from(thread_id * msgs_per_thread + i).unwrap() + 1),
                            };
                            if session.queue_message(msg).await.is_ok() {
                                queued += 1;
                            }
                        }
                        queued
                    };
                    join_set.spawn(task);
                }

                let mut total_queued = 0;
                while let Some(result) = join_set.join_next().await {
                    total_queued += result.unwrap();
                }

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

                for i in 0..operation_count {
                    match i % 5 {
                        0 => {
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
                                payload: vec![i.to_le_bytes()[0]],
                                qos: QoS::AtLeastOnce,
                                retain: false,
                                packet_id: Some(u16::try_from(i).unwrap() + 1),
                            };
                            let _ = session.queue_message(msg).await;
                        }
                        2 => {
                            let packet = publish_packet(u16::try_from(i).unwrap() + 1, QoS::AtLeastOnce);
                            let _ = session.store_unacked_publish(packet).await;
                        }
                        3 => {
                            let _ = session.stats().await;
                        }
                        4 => {
                            session.touch().await;
                        }
                        _ => unreachable!()
                    }
                }

                let elapsed = start.elapsed();
                let ops_per_second = f64::from(u32::try_from(operation_count).unwrap()) / elapsed.as_secs_f64();

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
