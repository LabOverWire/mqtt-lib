use crate::error::{MqttError, Result};
use crate::packet::publish::PublishPacket;
use crate::session::flow_control::{FlowControlManager, TopicAliasManager};
use crate::session::limits::LimitsManager;
use crate::session::queue::{MessageQueue, QueuedMessage};
#[cfg(not(target_arch = "wasm32"))]
use crate::session::quic_flow::{FlowRegistry, FlowState};
use crate::session::retained::{RetainedMessage, RetainedMessageStore};
use crate::session::subscription::{Subscription, SubscriptionManager};
use crate::time::{Duration, Instant};
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::flow::{FlowFlags, FlowId};
use crate::types::WillMessage;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Session configuration
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Session expiry interval in seconds (0 = session ends on disconnect)
    pub session_expiry_interval: u32,
    /// Maximum number of queued messages
    pub max_queued_messages: usize,
    /// Maximum size of queued messages in bytes
    pub max_queued_size: usize,
    /// Whether to persist session state
    pub persistent: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            session_expiry_interval: 0,
            max_queued_messages: 1000,
            max_queued_size: crate::constants::buffer::DEFAULT_CAPACITY
                * crate::constants::buffer::DEFAULT_CAPACITY, // 1MB
            persistent: false,
        }
    }
}

/// MQTT session state
#[derive(Debug)]
pub struct SessionState {
    /// Client identifier
    client_id: String,
    /// Session configuration
    config: SessionConfig,
    /// Subscriptions
    subscriptions: Arc<RwLock<SubscriptionManager>>,
    /// `QoS` 1 and 2 message queue
    message_queue: Arc<RwLock<MessageQueue>>,
    /// Unacknowledged PUBLISH packets (`packet_id` -> packet)
    unacked_publishes: Arc<RwLock<HashMap<u16, PublishPacket>>>,
    /// Unacknowledged PUBREL packets (`packet_id` -> timestamp)
    unacked_pubrels: Arc<RwLock<HashMap<u16, Instant>>>,
    #[cfg(not(target_arch = "wasm32"))]
    publish_flows: Arc<RwLock<HashMap<u16, FlowId>>>,
    /// Session creation time
    created_at: Instant,
    /// Last activity time
    last_activity: Arc<RwLock<Instant>>,
    /// Whether this is a clean session
    clean_start: bool,
    /// Flow control manager
    flow_control: Arc<RwLock<FlowControlManager>>,
    /// Topic alias manager for outgoing messages
    topic_alias_out: Arc<RwLock<TopicAliasManager>>,
    /// Topic alias manager for incoming messages
    topic_alias_in: Arc<RwLock<TopicAliasManager>>,
    /// Retained message store
    retained_messages: Arc<RetainedMessageStore>,
    /// Will message (to be published on abnormal disconnection)
    will_message: Arc<RwLock<Option<WillMessage>>>,
    /// Will delay timer handle
    will_delay_handle: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
    /// Limits manager for packet size and message expiry
    limits: Arc<RwLock<LimitsManager>>,
    #[cfg(not(target_arch = "wasm32"))]
    flow_registry: Arc<RwLock<FlowRegistry>>,
}

impl SessionState {
    /// Creates a new session state
    #[must_use]
    pub fn new(client_id: String, config: SessionConfig, clean_start: bool) -> Self {
        let now = Instant::now();
        Self {
            client_id,
            subscriptions: Arc::new(RwLock::new(SubscriptionManager::new())),
            message_queue: Arc::new(RwLock::new(MessageQueue::new(
                config.max_queued_messages,
                config.max_queued_size,
            ))),
            config,
            unacked_publishes: Arc::new(RwLock::new(HashMap::new())),
            unacked_pubrels: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(not(target_arch = "wasm32"))]
            publish_flows: Arc::new(RwLock::new(HashMap::new())),
            created_at: now,
            last_activity: Arc::new(RwLock::new(now)),
            clean_start,
            flow_control: Arc::new(RwLock::new(FlowControlManager::new(65535))), // Default to max
            topic_alias_out: Arc::new(RwLock::new(TopicAliasManager::new(0))), // Default to disabled
            topic_alias_in: Arc::new(RwLock::new(TopicAliasManager::new(0))), // Default to disabled
            retained_messages: Arc::new(RetainedMessageStore::new()),
            will_message: Arc::new(RwLock::new(None)),
            will_delay_handle: Arc::new(RwLock::new(None)),
            limits: Arc::new(RwLock::new(LimitsManager::with_defaults())),
            #[cfg(not(target_arch = "wasm32"))]
            flow_registry: Arc::new(RwLock::new(FlowRegistry::new(256))),
        }
    }

    #[must_use]
    /// Gets the client ID
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    #[must_use]
    /// Checks if this is a clean session
    pub fn is_clean(&self) -> bool {
        self.clean_start
    }

    /// Updates last activity time
    pub async fn touch(&self) {
        *self.last_activity.write().await = Instant::now();
    }

    /// Checks if session has expired
    pub async fn is_expired(&self) -> bool {
        if self.config.session_expiry_interval == 0 {
            return false; // Session doesn't expire
        }

        let last_activity = *self.last_activity.read().await;
        let expiry_duration = Duration::from_secs(u64::from(self.config.session_expiry_interval));
        last_activity.elapsed() > expiry_duration
    }

    /// Adds a subscription
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn add_subscription(
        &self,
        topic_filter: String,
        subscription: Subscription,
    ) -> Result<()> {
        self.touch().await;
        self.subscriptions
            .write()
            .await
            .add(topic_filter, subscription)
    }

    /// Removes a subscription
    ///
    /// Returns `Ok(true)` if the subscription existed and was removed,
    /// `Ok(false)` if the subscription did not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn remove_subscription(&self, topic_filter: &str) -> Result<bool> {
        self.touch().await;
        self.subscriptions.write().await.remove(topic_filter)
    }

    /// Gets subscriptions matching a topic
    pub async fn matching_subscriptions(&self, topic: &str) -> Vec<(String, Subscription)> {
        self.subscriptions
            .read()
            .await
            .matching_subscriptions(topic)
    }

    /// Gets all subscriptions
    pub async fn all_subscriptions(&self) -> HashMap<String, Subscription> {
        self.subscriptions.read().await.all()
    }

    /// Queues a message for delivery
    ///
    /// Returns information about the queue operation including whether the message
    /// was queued and how many messages were dropped to make room.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn queue_message(
        &self,
        message: QueuedMessage,
    ) -> Result<crate::session::queue::QueueResult> {
        self.touch().await;
        let limits = self.limits.read().await;
        let expiring_message = message.to_expiring(&limits);
        drop(limits);
        self.message_queue.write().await.enqueue(expiring_message)
    }

    /// Dequeues messages up to a limit
    pub async fn dequeue_messages(&self, limit: usize) -> Vec<QueuedMessage> {
        self.touch().await;
        self.message_queue
            .write()
            .await
            .dequeue_batch(limit)
            .into_iter()
            .map(|expiring| QueuedMessage {
                topic: expiring.topic,
                payload: expiring.payload,
                qos: expiring.qos,
                retain: expiring.retain,
                packet_id: expiring.packet_id,
            })
            .collect()
    }

    /// Gets the number of queued messages
    pub async fn queued_message_count(&self) -> usize {
        self.message_queue.read().await.len()
    }

    /// Stores an unacknowledged PUBLISH packet
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn store_unacked_publish(&self, packet: PublishPacket) -> Result<()> {
        if let Some(packet_id) = packet.packet_id {
            self.touch().await;
            self.unacked_publishes
                .write()
                .await
                .insert(packet_id, packet);
            Ok(())
        } else {
            Err(MqttError::ProtocolError(
                "PUBLISH packet missing packet ID".to_string(),
            ))
        }
    }

    /// Removes an acknowledged PUBLISH packet
    pub async fn remove_unacked_publish(&self, packet_id: u16) -> Option<PublishPacket> {
        self.touch().await;
        self.unacked_publishes.write().await.remove(&packet_id)
    }

    /// Gets all unacknowledged PUBLISH packets
    pub async fn get_unacked_publishes(&self) -> Vec<PublishPacket> {
        self.unacked_publishes
            .read()
            .await
            .values()
            .cloned()
            .collect()
    }

    /// Stores an unacknowledged PUBREL packet
    pub async fn store_unacked_pubrel(&self, packet_id: u16) {
        self.touch().await;
        self.unacked_pubrels
            .write()
            .await
            .insert(packet_id, Instant::now());
    }

    /// Removes an acknowledged PUBREL packet
    pub async fn remove_unacked_pubrel(&self, packet_id: u16) -> bool {
        self.touch().await;
        self.unacked_pubrels
            .write()
            .await
            .remove(&packet_id)
            .is_some()
    }

    /// Gets all unacknowledged PUBREL packet IDs
    pub async fn get_unacked_pubrels(&self) -> Vec<u16> {
        self.unacked_pubrels.read().await.keys().copied().collect()
    }

    /// Clears all session state
    pub async fn clear(&self) {
        self.subscriptions.write().await.clear();
        self.message_queue.write().await.clear();
        self.unacked_publishes.write().await.clear();
        self.unacked_pubrels.write().await.clear();
        #[cfg(not(target_arch = "wasm32"))]
        self.publish_flows.write().await.clear();
    }

    /// Gets session statistics
    pub async fn stats(&self) -> SessionStats {
        SessionStats {
            subscription_count: self.subscriptions.read().await.count(),
            queued_message_count: self.message_queue.read().await.len(),
            unacked_publish_count: self.unacked_publishes.read().await.len(),
            unacked_pubrel_count: self.unacked_pubrels.read().await.len(),
            uptime: self.created_at.elapsed(),
            last_activity: self.last_activity.read().await.elapsed(),
        }
    }

    /// Sets the receive maximum for flow control
    pub async fn set_receive_maximum(&self, receive_maximum: u16) {
        let mut flow_control = self.flow_control.write().await;
        flow_control.set_receive_maximum(receive_maximum).await;
    }

    /// Sets the topic alias maximum for outgoing messages
    pub async fn set_topic_alias_maximum_out(&self, max: u16) {
        let mut topic_alias = self.topic_alias_out.write().await;
        *topic_alias = TopicAliasManager::new(max);
    }

    /// Sets the topic alias maximum for incoming messages
    pub async fn set_topic_alias_maximum_in(&self, max: u16) {
        let mut topic_alias = self.topic_alias_in.write().await;
        *topic_alias = TopicAliasManager::new(max);
    }

    /// Checks if we can send a `QoS` 1/2 message according to flow control
    pub async fn can_send_qos_message(&self) -> bool {
        self.flow_control.read().await.can_send()
    }

    /// Registers a `QoS` 1/2 message as in-flight for flow control
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn register_in_flight(&self, packet_id: u16) -> Result<()> {
        self.flow_control
            .write()
            .await
            .register_send(packet_id)
            .await
    }

    /// Acknowledges a `QoS` 1/2 message for flow control
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn acknowledge_in_flight(&self, packet_id: u16) -> Result<()> {
        self.flow_control.write().await.acknowledge(packet_id).await
    }

    #[must_use]
    /// Gets the flow control manager
    pub fn flow_control(&self) -> &Arc<RwLock<FlowControlManager>> {
        &self.flow_control
    }

    #[must_use]
    /// Gets the outgoing topic alias manager
    pub fn topic_alias_out(&self) -> &Arc<RwLock<TopicAliasManager>> {
        &self.topic_alias_out
    }

    #[must_use]
    /// Gets the limits manager
    pub fn limits(&self) -> &Arc<RwLock<LimitsManager>> {
        &self.limits
    }

    /// Sets the server's maximum packet size from CONNACK
    pub async fn set_server_maximum_packet_size(&self, size: u32) {
        let mut limits = self.limits.write().await;
        limits.set_server_maximum_packet_size(size);
    }

    /// Sets the client's maximum packet size from `ConnectOptions`
    pub async fn set_client_maximum_packet_size(&self, size: u32) {
        let mut limits = self.limits.write().await;
        limits.set_client_maximum_packet_size(size);
    }

    /// Checks if a packet size is within limits
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn check_packet_size(&self, size: usize) -> Result<()> {
        self.limits.read().await.check_packet_size(size)
    }

    /// Gets the effective maximum packet size
    pub async fn effective_maximum_packet_size(&self) -> u32 {
        self.limits.read().await.effective_maximum_packet_size()
    }

    /// Gets or creates a topic alias for outgoing messages
    pub async fn get_or_create_topic_alias(&self, topic: &str) -> Option<u16> {
        self.topic_alias_out
            .write()
            .await
            .get_or_create_alias(topic)
    }

    /// Registers a topic alias from incoming messages
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn register_incoming_topic_alias(&self, alias: u16, topic: &str) -> Result<()> {
        self.topic_alias_in
            .write()
            .await
            .register_alias(alias, topic)
    }

    /// Gets the topic for an incoming alias
    pub async fn get_topic_for_alias(&self, alias: u16) -> Option<String> {
        self.topic_alias_in
            .read()
            .await
            .get_topic(alias)
            .map(String::from)
    }

    /// Removes expired messages from the queue
    pub async fn remove_expired_messages(&self, timeout: crate::time::Duration) {
        self.message_queue.write().await.remove_expired(timeout);
    }

    /// Stores or clears a retained message
    pub async fn store_retained_message(&self, packet: &PublishPacket) {
        let topic = packet.topic_name.clone();

        if packet.payload.is_empty() {
            // Empty payload clears the retained message
            self.retained_messages.store(topic, None).await;
        } else {
            // Store the retained message
            let message = RetainedMessage::from(packet);
            self.retained_messages.store(topic, Some(message)).await;
        }
    }

    /// Gets retained messages matching a topic filter
    pub async fn get_retained_messages(&self, topic_filter: &str) -> Vec<RetainedMessage> {
        self.retained_messages.get_matching(topic_filter).await
    }

    #[must_use]
    /// Gets the retained message store
    pub fn retained_messages(&self) -> &Arc<RetainedMessageStore> {
        &self.retained_messages
    }

    /// Sets the Will message for this session
    pub async fn set_will_message(&self, will: Option<WillMessage>) {
        let mut will_message = self.will_message.write().await;
        *will_message = will;
    }

    /// Gets the Will message for this session
    pub async fn will_message(&self) -> Option<WillMessage> {
        let will_message = self.will_message.read().await;
        will_message.clone()
    }

    /// Triggers Will message publication (called on abnormal disconnection)
    pub async fn trigger_will_message(&self) -> Option<WillMessage> {
        let mut will_message = self.will_message.write().await;
        let will = will_message.take(); // Remove the will message so it's only sent once

        if let Some(ref will) = will {
            // If there's a will delay interval, start the delay timer
            if let Some(delay_seconds) = will.properties.will_delay_interval {
                if delay_seconds > 0 {
                    let delay_handle_clone = Arc::clone(&self.will_delay_handle);

                    // Spawn a task to handle the delay
                    let handle = tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(u64::from(delay_seconds))).await;
                        // The actual will message publication will be handled by the caller
                        // This just implements the delay
                    });

                    let mut delay_handle = delay_handle_clone.write().await;
                    *delay_handle = Some(handle);

                    // Return None to indicate the will should be delayed
                    return None;
                }
            }
        }

        will
    }

    /// Cancels the Will message (called on normal disconnection)
    pub async fn cancel_will_message(&self) {
        // Clear the will message
        let mut will_message = self.will_message.write().await;
        *will_message = None;

        // Cancel any pending will delay timer
        let mut delay_handle = self.will_delay_handle.write().await;
        if let Some(handle) = delay_handle.take() {
            handle.abort();
        }
    }

    /// Checks if the will delay has completed
    pub async fn is_will_delay_complete(&self) -> bool {
        let delay_handle = self.will_delay_handle.read().await;
        if let Some(ref handle) = *delay_handle {
            handle.is_finished()
        } else {
            true // No delay, so it's "complete"
        }
    }

    /// Complete a publish (`QoS` 1 - after PUBACK received)
    pub async fn complete_publish(&self, packet_id: u16) {
        self.touch().await;
        self.unacked_publishes.write().await.remove(&packet_id);
    }

    /// Store PUBREC for `QoS` 2 flow
    pub async fn store_pubrec(&self, packet_id: u16) {
        self.touch().await;
        // For incoming QoS 2 messages, we need to track that we've sent PUBREC
        // and are waiting for PUBREL. Store the packet ID with the current timestamp.
        self.unacked_pubrels
            .write()
            .await
            .insert(packet_id, Instant::now());
    }

    /// Check if we have a stored PUBREC for the given packet ID
    pub async fn has_pubrec(&self, packet_id: u16) -> bool {
        // Check if there's an unacked PUBREL with this packet ID
        // For received QoS 2 messages, this indicates we sent PUBREC and are waiting for PUBREL
        self.unacked_pubrels.read().await.contains_key(&packet_id)
    }

    /// Remove PUBREC state for the given packet ID (called when handling PUBREL)
    pub async fn remove_pubrec(&self, packet_id: u16) {
        self.touch().await;
        // Remove the stored PUBREL state as we're completing the QoS 2 flow
        self.unacked_pubrels.write().await.remove(&packet_id);
    }

    /// Store PUBREL for `QoS` 2 flow
    pub async fn store_pubrel(&self, packet_id: u16) {
        self.touch().await;
        self.unacked_pubrels
            .write()
            .await
            .insert(packet_id, Instant::now());
    }

    /// Complete PUBREC (after sending PUBREL)
    pub async fn complete_pubrec(&self, packet_id: u16) {
        self.touch().await;
        // Remove from unacked publishes as we've moved to PUBREL phase
        self.unacked_publishes.write().await.remove(&packet_id);
    }

    /// Complete PUBREL (after receiving PUBCOMP)
    pub async fn complete_pubrel(&self, packet_id: u16) {
        self.touch().await;
        self.unacked_pubrels.write().await.remove(&packet_id);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn flow_registry(&self) -> &Arc<RwLock<FlowRegistry>> {
        &self.flow_registry
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn register_flow(&self, state: FlowState) -> bool {
        self.touch().await;
        self.flow_registry.write().await.register_flow(state)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn create_client_flow(
        &self,
        flags: FlowFlags,
        expire_interval: Option<std::time::Duration>,
    ) -> Option<FlowId> {
        self.touch().await;
        self.flow_registry
            .write()
            .await
            .new_client_flow(flags, expire_interval)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn get_flow(&self, id: FlowId) -> Option<FlowState> {
        self.flow_registry.read().await.get(id).cloned()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn remove_flow(&self, id: FlowId) -> Option<FlowState> {
        self.touch().await;
        self.flow_registry.write().await.remove(id)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn touch_flow(&self, id: FlowId) {
        self.flow_registry.write().await.touch(id);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn clear_flows(&self) {
        self.flow_registry.write().await.clear();
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn get_all_flow_ids(&self) -> Vec<FlowId> {
        self.flow_registry
            .read()
            .await
            .iter()
            .map(|(id, _)| *id)
            .collect()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn get_recoverable_flows(&self) -> Vec<(FlowId, FlowFlags)> {
        self.flow_registry
            .read()
            .await
            .iter()
            .filter(|(_, state)| !state.is_expired())
            .map(|(id, state)| (*id, state.flags))
            .collect()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn expire_flows(&self) -> Vec<FlowId> {
        self.flow_registry.write().await.expire_flows()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn flow_count(&self) -> usize {
        self.flow_registry.read().await.len()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn store_publish_flow(&self, packet_id: u16, flow_id: FlowId) {
        self.touch().await;
        self.publish_flows.write().await.insert(packet_id, flow_id);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn get_publish_flow(&self, packet_id: u16) -> Option<FlowId> {
        self.publish_flows.read().await.get(&packet_id).copied()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn remove_publish_flow(&self, packet_id: u16) -> Option<FlowId> {
        self.touch().await;
        self.publish_flows.write().await.remove(&packet_id)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn clear_publish_flows(&self) {
        self.publish_flows.write().await.clear();
    }
}

/// Session statistics
#[derive(Debug, Clone)]
pub struct SessionStats {
    /// Number of active subscriptions
    pub subscription_count: usize,
    /// Number of queued messages
    pub queued_message_count: usize,
    /// Number of unacknowledged PUBLISH packets
    pub unacked_publish_count: usize,
    /// Number of unacknowledged PUBREL packets
    pub unacked_pubrel_count: usize,
    /// Session uptime
    pub uptime: Duration,
    /// Time since last activity
    pub last_activity: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::subscribe::SubscriptionOptions;
    use crate::types::{WillMessage, WillProperties};
    use crate::{Properties, QoS};

    #[tokio::test]
    async fn test_session_creation() {
        let config = SessionConfig::default();
        let session = SessionState::new("test-client".to_string(), config, true);

        assert_eq!(session.client_id(), "test-client");
        assert!(session.is_clean());
        assert!(!session.is_expired().await);
    }

    #[tokio::test]
    async fn test_session_expiry() {
        let config = SessionConfig {
            session_expiry_interval: 1, // 1 second
            ..Default::default()
        };
        let session = SessionState::new("test-client".to_string(), config, false);

        // Initially not expired
        assert!(!session.is_expired().await);

        // Update last activity to past
        *session.last_activity.write().await =
            Instant::now().checked_sub(Duration::from_secs(2)).unwrap();

        // Now should be expired
        assert!(session.is_expired().await);
    }

    #[tokio::test]
    async fn test_subscription_management() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        let sub = Subscription {
            topic_filter: "test/topic".to_string(),
            options: SubscriptionOptions::default(),
        };

        // Add subscription
        session
            .add_subscription("test/topic".to_string(), sub.clone())
            .await
            .unwrap();

        // Check matching
        let matches = session.matching_subscriptions("test/topic").await;
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, "test/topic");

        // Remove subscription
        session.remove_subscription("test/topic").await.unwrap();
        let matches = session.matching_subscriptions("test/topic").await;
        assert_eq!(matches.len(), 0);
    }

    #[tokio::test]
    async fn test_message_queueing() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Queue messages
        let msg1 = QueuedMessage {
            topic: "test/1".to_string(),
            payload: vec![1, 2, 3],
            qos: QoS::AtLeastOnce,
            retain: false,
            packet_id: Some(1),
        };

        let msg2 = QueuedMessage {
            topic: "test/2".to_string(),
            payload: vec![4, 5, 6],
            qos: QoS::AtMostOnce,
            retain: false,
            packet_id: None,
        };

        session.queue_message(msg1).await.unwrap();
        session.queue_message(msg2).await.unwrap();

        assert_eq!(session.queued_message_count().await, 2);

        // Dequeue messages
        let messages = session.dequeue_messages(1).await;
        assert_eq!(messages.len(), 1);
        assert_eq!(session.queued_message_count().await, 1);
    }

    #[tokio::test]
    async fn test_unacked_publish_tracking() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Create a publish packet
        let packet = PublishPacket {
            topic_name: "test/topic".to_string(),
            packet_id: Some(123),
            payload: vec![1, 2, 3],
            qos: QoS::AtLeastOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
        };

        // Store unacked publish
        session.store_unacked_publish(packet.clone()).await.unwrap();

        let unacked = session.get_unacked_publishes().await;
        assert_eq!(unacked.len(), 1);
        assert_eq!(unacked[0].packet_id, Some(123));

        // Remove acknowledged publish
        let removed = session.remove_unacked_publish(123).await;
        assert!(removed.is_some());
        assert_eq!(session.get_unacked_publishes().await.len(), 0);
    }

    #[tokio::test]
    async fn test_unacked_pubrel_tracking() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Store unacked pubrels
        session.store_unacked_pubrel(100).await;
        session.store_unacked_pubrel(101).await;

        let pubrels = session.get_unacked_pubrels().await;
        assert_eq!(pubrels.len(), 2);
        assert!(pubrels.contains(&100));
        assert!(pubrels.contains(&101));

        // Remove acknowledged pubrel
        assert!(session.remove_unacked_pubrel(100).await);
        assert_eq!(session.get_unacked_pubrels().await.len(), 1);
    }

    #[tokio::test]
    async fn test_session_clear() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Add some data
        let sub = Subscription {
            topic_filter: "test/#".to_string(),
            options: SubscriptionOptions::default(),
        };
        session
            .add_subscription("test/#".to_string(), sub)
            .await
            .unwrap();

        let msg = QueuedMessage {
            topic: "test".to_string(),
            payload: vec![1],
            qos: QoS::AtMostOnce,
            retain: false,
            packet_id: None,
        };
        session.queue_message(msg).await.unwrap();

        session.store_unacked_pubrel(1).await;

        // Clear session
        session.clear().await;

        // Verify everything is cleared
        assert_eq!(session.all_subscriptions().await.len(), 0);
        assert_eq!(session.queued_message_count().await, 0);
        assert_eq!(session.get_unacked_pubrels().await.len(), 0);
    }

    #[tokio::test]
    async fn test_session_stats() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Add some data
        let sub = Subscription {
            topic_filter: "test/#".to_string(),
            options: SubscriptionOptions::default(),
        };
        session
            .add_subscription("test/#".to_string(), sub)
            .await
            .unwrap();

        let stats = session.stats().await;
        assert_eq!(stats.subscription_count, 1);
        assert_eq!(stats.queued_message_count, 0);
        // Uptime might be 0 on very fast systems, so just check it exists
        let _ = stats.uptime.as_nanos();
    }

    #[tokio::test]
    async fn test_flow_control_integration() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Set receive maximum
        session.set_receive_maximum(2).await;

        // Should be able to send initially
        assert!(session.can_send_qos_message().await);

        // Register in-flight messages
        session.register_in_flight(1).await.unwrap();
        session.register_in_flight(2).await.unwrap();

        // Should not be able to send more
        assert!(!session.can_send_qos_message().await);

        // Try to register another
        assert!(session.register_in_flight(3).await.is_err());

        // Acknowledge one
        session.acknowledge_in_flight(1).await.unwrap();

        // Should be able to send again
        assert!(session.can_send_qos_message().await);
    }

    #[tokio::test]
    async fn test_topic_alias_integration() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Set topic alias maximum
        session.set_topic_alias_maximum_out(10).await;
        session.set_topic_alias_maximum_in(10).await;

        // Get or create alias for outgoing
        let alias1 = session.get_or_create_topic_alias("topic/1").await;
        assert_eq!(alias1, Some(1));

        // Same topic should get same alias
        let alias1_again = session.get_or_create_topic_alias("topic/1").await;
        assert_eq!(alias1_again, Some(1));

        // Register incoming alias
        session
            .register_incoming_topic_alias(5, "incoming/topic")
            .await
            .unwrap();

        // Get topic for alias
        let topic = session.get_topic_for_alias(5).await;
        assert_eq!(topic, Some("incoming/topic".to_string()));
    }

    #[tokio::test]
    async fn test_session_expiry_zero_interval() {
        let config = SessionConfig {
            session_expiry_interval: 0, // Session doesn't expire
            ..Default::default()
        };
        let session = SessionState::new("test-client".to_string(), config, false);

        // Update last activity to past
        *session.last_activity.write().await = Instant::now()
            .checked_sub(Duration::from_secs(100))
            .unwrap();

        // Should not be expired
        assert!(!session.is_expired().await);
    }

    #[tokio::test]
    async fn test_wildcard_subscriptions() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        let sub1 = Subscription {
            topic_filter: "test/+/topic".to_string(),
            options: SubscriptionOptions::default(),
        };

        let sub2 = Subscription {
            topic_filter: "test/#".to_string(),
            options: SubscriptionOptions::default(),
        };

        session
            .add_subscription("test/+/topic".to_string(), sub1)
            .await
            .unwrap();
        session
            .add_subscription("test/#".to_string(), sub2)
            .await
            .unwrap();

        // Check matching
        let matches = session.matching_subscriptions("test/foo/topic").await;
        assert_eq!(matches.len(), 2);

        let all_subs = session.all_subscriptions().await;
        assert_eq!(all_subs.len(), 2);
    }

    #[tokio::test]
    async fn test_message_queue_limits() {
        let config = SessionConfig {
            max_queued_messages: 2,
            max_queued_size: 100,
            ..Default::default()
        };

        let session = SessionState::new("test-client".to_string(), config, true);

        // Queue messages up to limit
        let msg1 = QueuedMessage {
            topic: "test/1".to_string(),
            payload: vec![0; 40],
            qos: QoS::AtLeastOnce,
            retain: false,
            packet_id: Some(1),
        };

        let msg2 = QueuedMessage {
            topic: "test/2".to_string(),
            payload: vec![0; 40],
            qos: QoS::AtLeastOnce,
            retain: false,
            packet_id: Some(2),
        };

        let msg3 = QueuedMessage {
            topic: "test/3".to_string(),
            payload: vec![0; 40],
            qos: QoS::AtLeastOnce,
            retain: false,
            packet_id: Some(3),
        };

        session.queue_message(msg1).await.unwrap();
        session.queue_message(msg2).await.unwrap();

        // Third message should succeed but drop the oldest message
        session.queue_message(msg3).await.unwrap();

        // Should still have 2 messages
        assert_eq!(session.queued_message_count().await, 2);

        // Dequeue all and verify oldest was dropped
        let messages = session.dequeue_messages(3).await;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].topic, "test/2");
        assert_eq!(messages[1].topic, "test/3");
    }

    #[tokio::test]
    async fn test_unacked_publish_no_packet_id() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Create a publish packet without packet ID
        let packet = PublishPacket {
            topic_name: "test/topic".to_string(),
            packet_id: None,
            payload: vec![1, 2, 3],
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
        };

        // Should fail to store
        assert!(session.store_unacked_publish(packet).await.is_err());
    }

    #[tokio::test]
    async fn test_qos2_flow() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Store publish for QoS 2
        let packet = PublishPacket {
            topic_name: "test/topic".to_string(),
            packet_id: Some(123),
            payload: vec![1, 2, 3],
            qos: QoS::ExactlyOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
        };

        session.store_unacked_publish(packet).await.unwrap();

        // Store PUBREC (doesn't remove publish yet)
        session.store_pubrec(123).await;
        assert_eq!(session.get_unacked_publishes().await.len(), 1);

        // Complete PUBREC (removes publish, adds to pubrel)
        session.complete_pubrec(123).await;
        session.store_pubrel(123).await;
        assert_eq!(session.get_unacked_publishes().await.len(), 0);
        assert_eq!(session.get_unacked_pubrels().await.len(), 1);

        // Complete PUBREL
        session.complete_pubrel(123).await;
        assert_eq!(session.get_unacked_pubrels().await.len(), 0);
    }

    #[tokio::test]
    async fn test_packet_size_limits() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Set server maximum packet size
        session.set_server_maximum_packet_size(1000).await;

        // Check packet size within limit
        assert!(session.check_packet_size(500).await.is_ok());

        // Check packet size exceeds limit
        assert!(session.check_packet_size(1001).await.is_err());

        // Get effective maximum
        assert_eq!(session.effective_maximum_packet_size().await, 1000);
    }

    #[tokio::test]
    async fn test_retained_messages() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Store retained message
        let packet1 = PublishPacket {
            topic_name: "test/retained".to_string(),
            packet_id: None,
            payload: vec![1, 2, 3],
            qos: QoS::AtMostOnce,
            retain: true,
            dup: false,
            properties: Properties::default(),
        };

        session.store_retained_message(&packet1).await;

        // Get retained messages
        let retained = session.get_retained_messages("test/retained").await;
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].payload, vec![1, 2, 3]);

        // Clear retained message with empty payload
        let packet2 = PublishPacket {
            topic_name: "test/retained".to_string(),
            packet_id: None,
            payload: vec![],
            qos: QoS::AtMostOnce,
            retain: true,
            dup: false,
            properties: Properties::default(),
        };

        session.store_retained_message(&packet2).await;

        // Should be cleared
        let retained = session.get_retained_messages("test/retained").await;
        assert_eq!(retained.len(), 0);
    }

    #[tokio::test]
    async fn test_retained_message_wildcard_matching() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Store multiple retained messages
        let packet1 = PublishPacket {
            topic_name: "test/device1/status".to_string(),
            packet_id: None,
            payload: b"online".to_vec(),
            qos: QoS::AtMostOnce,
            retain: true,
            dup: false,
            properties: Properties::default(),
        };

        let packet2 = PublishPacket {
            topic_name: "test/device2/status".to_string(),
            packet_id: None,
            payload: b"offline".to_vec(),
            qos: QoS::AtMostOnce,
            retain: true,
            dup: false,
            properties: Properties::default(),
        };

        session.store_retained_message(&packet1).await;
        session.store_retained_message(&packet2).await;

        // Get with wildcard
        let retained = session.get_retained_messages("test/+/status").await;
        assert_eq!(retained.len(), 2);
    }

    #[tokio::test]
    async fn test_will_message() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Set will message
        let will = WillMessage {
            topic: "test/will".to_string(),
            payload: b"disconnected".to_vec(),
            qos: QoS::AtLeastOnce,
            retain: false,
            properties: WillProperties::default(),
        };

        session.set_will_message(Some(will.clone())).await;

        // Get will message
        let stored_will = session.will_message().await;
        assert!(stored_will.is_some());
        assert_eq!(stored_will.unwrap().topic, "test/will");

        // Trigger will message (abnormal disconnection)
        let triggered = session.trigger_will_message().await;
        assert!(triggered.is_some());

        // Will should be cleared after triggering
        assert!(session.will_message().await.is_none());
    }

    #[tokio::test]
    async fn test_will_message_cancellation() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Set will message
        let will = WillMessage {
            topic: "test/will".to_string(),
            payload: b"disconnected".to_vec(),
            qos: QoS::AtLeastOnce,
            retain: false,
            properties: WillProperties::default(),
        };

        session.set_will_message(Some(will)).await;

        // Cancel will message (normal disconnection)
        session.cancel_will_message().await;

        // Will should be cleared
        assert!(session.will_message().await.is_none());
    }

    #[tokio::test]
    async fn test_will_delay() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Set will message with delay
        let will_props = WillProperties {
            will_delay_interval: Some(1), // 1 second delay
            ..Default::default()
        };

        let will = WillMessage {
            topic: "test/will".to_string(),
            payload: b"disconnected".to_vec(),
            qos: QoS::AtLeastOnce,
            retain: false,
            properties: will_props,
        };

        session.set_will_message(Some(will)).await;

        // Trigger will with delay
        let triggered = session.trigger_will_message().await;
        assert!(triggered.is_none()); // Should return None due to delay

        // Check delay is not complete yet
        assert!(!session.is_will_delay_complete().await);

        // Wait for delay
        tokio::time::sleep(Duration::from_millis(1100)).await;

        // Now delay should be complete
        assert!(session.is_will_delay_complete().await);
    }

    #[tokio::test]
    async fn test_touch_updates_activity() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        let initial_activity = *session.last_activity.read().await;

        // Wait a bit
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Touch should update activity
        session.touch().await;

        let new_activity = *session.last_activity.read().await;
        assert!(new_activity > initial_activity);
    }

    #[tokio::test]
    async fn test_activity_tracking_on_operations() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        let initial_activity = *session.last_activity.read().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Various operations should update activity
        let sub = Subscription {
            topic_filter: "test".to_string(),
            options: SubscriptionOptions::default(),
        };
        session
            .add_subscription("test".to_string(), sub)
            .await
            .unwrap();

        let activity_after_sub = *session.last_activity.read().await;
        assert!(activity_after_sub > initial_activity);
    }

    #[tokio::test]
    async fn test_complete_publish_flow() {
        let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

        // Store unacked publish
        let packet = PublishPacket {
            topic_name: "test/topic".to_string(),
            packet_id: Some(100),
            payload: vec![1, 2, 3],
            qos: QoS::AtLeastOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
        };

        session.store_unacked_publish(packet).await.unwrap();
        assert_eq!(session.get_unacked_publishes().await.len(), 1);

        // Complete publish
        session.complete_publish(100).await;
        assert_eq!(session.get_unacked_publishes().await.len(), 0);
    }
}
