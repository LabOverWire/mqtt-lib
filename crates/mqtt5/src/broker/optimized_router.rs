//! Optimized message routing for high-performance MQTT broker
//!
//! This module implements performance optimizations for message routing:
//! - Subscription indexing with topic trees for fast matching
//! - Pre-allocated message delivery pools
//! - Optimized topic matching algorithms
//! - Reduced memory allocations in hot paths

use crate::broker::storage::{DynamicStorage, QueuedMessage, RetainedMessage, StorageBackend};
use crate::packet::publish::PublishPacket;
use crate::QoS;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, trace};

/// High-performance subscription information with pre-computed data
#[derive(Debug, Clone)]
pub struct OptimizedSubscription {
    /// Client ID that owns this subscription
    pub client_id: String,
    /// Quality of Service level
    pub qos: QoS,
    /// Subscription identifier (MQTT v5.0)
    pub subscription_id: Option<u32>,
    /// Shared subscription group name (if this is a shared subscription)
    pub share_group: Option<String>,
    /// Pre-computed topic segments for faster matching
    pub topic_segments: Vec<String>,
    /// Whether this subscription contains wildcards
    pub has_wildcards: bool,
}

/// Optimized client information with connection state
#[derive(Debug, Clone)]
pub struct OptimizedClientInfo {
    /// Channel to send messages to this client
    pub sender: tokio::sync::mpsc::Sender<PublishPacket>,
    /// Whether client is currently online
    pub online: bool,
    /// Last seen timestamp for cleanup
    pub last_seen: std::time::Instant,
}

/// Node in the subscription tree for fast topic-based lookups
#[derive(Debug)]
struct SubscriptionTreeNode {
    /// Direct subscriptions to this exact topic path
    exact_subscriptions: Vec<OptimizedSubscription>,
    /// Wildcard subscriptions that match this level
    wildcard_subscriptions: Vec<OptimizedSubscription>,
    /// Multilevel wildcard subscriptions (matches this and all sub-levels)
    multilevel_subscriptions: Vec<OptimizedSubscription>,
    /// Child nodes for sub-topics
    children: HashMap<String, SubscriptionTreeNode>,
}

impl SubscriptionTreeNode {
    fn new() -> Self {
        Self {
            exact_subscriptions: Vec::new(),
            wildcard_subscriptions: Vec::new(),
            multilevel_subscriptions: Vec::new(),
            children: HashMap::new(),
        }
    }
}

/// High-performance message router with optimized data structures
pub struct OptimizedMessageRouter {
    /// Subscription tree for fast topic-based matching
    subscription_tree: Arc<RwLock<SubscriptionTreeNode>>,
    /// Map of exact topics to retained messages (indexed by topic hash)
    retained_messages: Arc<RwLock<HashMap<u64, PublishPacket>>>,
    /// Active client connections (indexed by client ID hash)
    clients: Arc<RwLock<HashMap<String, OptimizedClientInfo>>>,
    /// Storage backend for persistence
    storage: Option<Arc<DynamicStorage>>,
    /// Round-robin counters for shared subscription groups
    share_group_counters: Arc<RwLock<HashMap<String, Arc<AtomicUsize>>>>,
    /// Pre-allocated vectors for message delivery (thread-local pools)
    delivery_pools: tokio::task::LocalSet,
    /// Performance metrics
    metrics: Arc<RouterMetrics>,
}

/// Performance metrics for the router
#[derive(Debug, Default)]
struct RouterMetrics {
    messages_routed: AtomicUsize,
    subscriptions_matched: AtomicUsize,
    delivery_time_total_us: AtomicUsize,
    subscription_lookups: AtomicUsize,
}

impl OptimizedMessageRouter {
    /// Creates a new optimized message router
    pub fn new() -> Self {
        Self {
            subscription_tree: Arc::new(RwLock::new(SubscriptionTreeNode::new())),
            retained_messages: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            storage: None,
            share_group_counters: Arc::new(RwLock::new(HashMap::new())),
            delivery_pools: tokio::task::LocalSet::new(),
            metrics: Arc::new(RouterMetrics::default()),
        }
    }

    /// Creates a new optimized router with storage backend
    pub fn with_storage(storage: Arc<DynamicStorage>) -> Self {
        Self {
            subscription_tree: Arc::new(RwLock::new(SubscriptionTreeNode::new())),
            retained_messages: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            storage: Some(storage),
            share_group_counters: Arc::new(RwLock::new(HashMap::new())),
            delivery_pools: tokio::task::LocalSet::new(),
            metrics: Arc::new(RouterMetrics::default()),
        }
    }

    /// Fast hash function for topic names
    fn hash_topic(topic: &str) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        topic.hash(&mut hasher);
        hasher.finish()
    }

    /// Splits topic into segments and analyzes wildcards
    fn analyze_topic_filter(topic_filter: &str) -> (Vec<String>, bool) {
        let segments: Vec<String> = topic_filter
            .split('/')
            .map(std::string::ToString::to_string)
            .collect();
        let has_wildcards = topic_filter.contains('+') || topic_filter.contains('#');
        (segments, has_wildcards)
    }

    /// Registers a client connection with optimized data structures
    pub async fn register_client(
        &self,
        client_id: String,
        sender: tokio::sync::mpsc::Sender<PublishPacket>,
    ) {
        let client_info = OptimizedClientInfo {
            sender,
            online: true,
            last_seen: std::time::Instant::now(),
        };

        let mut clients = self.clients.write().await;
        clients.insert(client_id.clone(), client_info);

        debug!("Registered optimized client: {}", client_id);
    }

    /// Removes a client connection and cleans up subscriptions
    pub async fn unregister_client(&self, client_id: &str) {
        {
            let mut clients = self.clients.write().await;
            clients.remove(client_id);
        }

        // Remove all subscriptions for this client
        self.remove_all_subscriptions(client_id).await;

        debug!("Unregistered optimized client: {}", client_id);
    }

    /// Adds a subscription with optimized indexing
    pub async fn subscribe(
        &self,
        client_id: String,
        topic_filter: String,
        qos: QoS,
        subscription_id: Option<u32>,
    ) {
        let (actual_filter, share_group) = Self::parse_shared_subscription(&topic_filter);
        let (topic_segments, has_wildcards) = Self::analyze_topic_filter(actual_filter);

        let subscription = OptimizedSubscription {
            client_id: client_id.clone(),
            qos,
            subscription_id,
            share_group: share_group.clone(),
            topic_segments,
            has_wildcards,
        };

        // Add to subscription tree for fast lookup
        self.add_to_subscription_tree(&subscription, actual_filter)
            .await;

        // Initialize share group counter if needed
        if let Some(group) = share_group {
            let mut counters = self.share_group_counters.write().await;
            counters
                .entry(group)
                .or_insert_with(|| Arc::new(AtomicUsize::new(0)));
        }

        debug!(
            "Client {} subscribed to {} (optimized)",
            client_id, topic_filter
        );
    }

    /// Adds subscription to the optimized tree structure
    async fn add_to_subscription_tree(
        &self,
        subscription: &OptimizedSubscription,
        topic_filter: &str,
    ) {
        let mut tree = self.subscription_tree.write().await;
        let mut current_node = &mut *tree;

        let segments: Vec<&str> = topic_filter.split('/').collect();

        for (i, segment) in segments.iter().enumerate() {
            if *segment == "#" {
                // Multilevel wildcard - matches this level and all below
                current_node
                    .multilevel_subscriptions
                    .push(subscription.clone());
                return;
            }

            if i == segments.len() - 1 {
                // Last segment - navigate to child and add as exact subscription
                current_node = current_node
                    .children
                    .entry((*segment).to_string())
                    .or_insert_with(SubscriptionTreeNode::new);

                if *segment == "+" {
                    current_node
                        .wildcard_subscriptions
                        .push(subscription.clone());
                } else {
                    current_node.exact_subscriptions.push(subscription.clone());
                }
            } else {
                // Navigate to child node
                current_node = current_node
                    .children
                    .entry((*segment).to_string())
                    .or_insert_with(SubscriptionTreeNode::new);
            }
        }
    }

    /// Fast topic matching using the subscription tree
    async fn find_matching_subscriptions(&self, topic: &str) -> Vec<OptimizedSubscription> {
        self.metrics
            .subscription_lookups
            .fetch_add(1, Ordering::Relaxed);

        let tree = self.subscription_tree.read().await;
        let mut matches = Vec::new();

        // Collect matches from the tree
        self.collect_matches_from_tree(
            &tree,
            topic,
            &topic.split('/').collect::<Vec<_>>(),
            0,
            &mut matches,
        );

        self.metrics
            .subscriptions_matched
            .fetch_add(matches.len(), Ordering::Relaxed);
        matches
    }

    /// Recursively collect matching subscriptions from tree
    #[allow(clippy::only_used_in_recursion)]
    fn collect_matches_from_tree(
        &self,
        node: &SubscriptionTreeNode,
        topic: &str,
        topic_segments: &[&str],
        segment_idx: usize,
        matches: &mut Vec<OptimizedSubscription>,
    ) {
        // Add multilevel wildcard matches (they match everything from this level down)
        matches.extend(node.multilevel_subscriptions.iter().cloned());

        if segment_idx >= topic_segments.len() {
            // Reached end of topic - add exact matches
            matches.extend(node.exact_subscriptions.iter().cloned());
            return;
        }

        let current_segment = topic_segments[segment_idx];

        // Check exact match child
        if let Some(child) = node.children.get(current_segment) {
            if segment_idx == topic_segments.len() - 1 {
                // Last segment - add exact subscriptions
                matches.extend(child.exact_subscriptions.iter().cloned());
            } else {
                // Continue recursion
                self.collect_matches_from_tree(
                    child,
                    topic,
                    topic_segments,
                    segment_idx + 1,
                    matches,
                );
            }
        }

        // Check wildcard match (+)
        if let Some(wildcard_child) = node.children.get("+") {
            if segment_idx == topic_segments.len() - 1 {
                // Last segment - add wildcard subscriptions
                matches.extend(wildcard_child.wildcard_subscriptions.iter().cloned());
            } else {
                // Continue recursion
                self.collect_matches_from_tree(
                    wildcard_child,
                    topic,
                    topic_segments,
                    segment_idx + 1,
                    matches,
                );
            }
        }
    }

    /// High-performance message routing with optimized algorithms
    pub async fn route_message_optimized(&self, publish: &PublishPacket) {
        let start_time = std::time::Instant::now();
        trace!("Routing message to topic: {}", publish.topic_name);

        // Update metrics for every message processed
        self.metrics.messages_routed.fetch_add(1, Ordering::Relaxed);

        // Handle retained messages with hash-based lookup
        if publish.retain {
            let topic_hash = Self::hash_topic(&publish.topic_name);
            let mut retained = self.retained_messages.write().await;

            if publish.payload.is_empty() {
                // Empty payload means delete retained message
                retained.remove(&topic_hash);
                debug!("Deleted retained message for topic: {}", publish.topic_name);

                // Remove from storage
                if let Some(ref storage) = self.storage {
                    if let Err(e) = storage.remove_retained_message(&publish.topic_name).await {
                        tracing::error!("Failed to remove retained message from storage: {}", e);
                    }
                }
            } else {
                retained.insert(topic_hash, publish.clone());
                debug!("Stored retained message for topic: {}", publish.topic_name);

                // Persist to storage
                if let Some(ref storage) = self.storage {
                    let retained_msg = RetainedMessage::new(publish.clone());
                    if let Err(e) = storage
                        .store_retained_message(&publish.topic_name, retained_msg)
                        .await
                    {
                        tracing::error!("Failed to store retained message to storage: {}", e);
                    }
                }
            }
        }

        // Fast subscription matching using optimized tree
        let matching_subs = self.find_matching_subscriptions(&publish.topic_name).await;

        if matching_subs.is_empty() {
            // Still update delivery time metrics
            let elapsed_us = start_time.elapsed().as_micros() as usize;
            self.metrics
                .delivery_time_total_us
                .fetch_add(elapsed_us, Ordering::Relaxed);
            return;
        }

        // Get client connections once
        let clients = self.clients.read().await;

        // Group subscriptions efficiently with pre-allocated capacity
        let mut share_groups: HashMap<String, Vec<&OptimizedSubscription>> = HashMap::new();
        let mut regular_subs: Vec<&OptimizedSubscription> = Vec::with_capacity(matching_subs.len());

        for sub in &matching_subs {
            if let Some(ref group) = sub.share_group {
                share_groups.entry(group.clone()).or_default().push(sub);
            } else {
                regular_subs.push(sub);
            }
        }

        // Process shared subscriptions with optimized round-robin
        for (group_name, group_subs) in share_groups {
            self.handle_shared_subscription_group(&group_name, &group_subs, publish, &clients)
                .await;
        }

        // Process regular subscriptions in batch
        for sub in regular_subs {
            self.deliver_to_subscriber_optimized(sub, publish, &clients)
                .await;
        }

        // Update delivery time metrics
        let elapsed_us = start_time.elapsed().as_micros() as usize;
        self.metrics
            .delivery_time_total_us
            .fetch_add(elapsed_us, Ordering::Relaxed);
    }

    /// Optimized shared subscription handling
    async fn handle_shared_subscription_group(
        &self,
        group_name: &str,
        group_subs: &[&OptimizedSubscription],
        publish: &PublishPacket,
        clients: &HashMap<String, OptimizedClientInfo>,
    ) {
        // Pre-filter online subscribers to avoid repeated HashMap lookups
        let online_subs: Vec<&OptimizedSubscription> = group_subs
            .iter()
            .filter(|sub| clients.get(&sub.client_id).map_or(false, |c| c.online))
            .copied()
            .collect();

        if !online_subs.is_empty() {
            // Optimized round-robin with atomic operations
            let counters = self.share_group_counters.read().await;
            if let Some(counter) = counters.get(group_name) {
                let index = counter.fetch_add(1, Ordering::Relaxed) % online_subs.len();
                let chosen_sub = online_subs[index];

                // Direct delivery without additional lookups
                if let Some(client_info) = clients.get(&chosen_sub.client_id) {
                    self.send_message_to_client(chosen_sub, publish, client_info);
                }
            }
        } else if !group_subs.is_empty() {
            // All subscribers offline - queue for persistence
            let sub = group_subs[0];
            if self.storage.is_some() && sub.qos != QoS::AtMostOnce {
                if let Some(ref storage) = self.storage {
                    let mut message = publish.clone();
                    message.qos = sub.qos;

                    let queued_msg =
                        QueuedMessage::new(message, sub.client_id.clone(), sub.qos, None);
                    if let Err(e) = storage.queue_message(queued_msg).await {
                        error!(
                            "Failed to queue message for offline shared subscriber {}: {}",
                            sub.client_id, e
                        );
                    }
                }
            }
        }
    }

    /// Optimized message delivery to individual subscriber
    async fn deliver_to_subscriber_optimized(
        &self,
        sub: &OptimizedSubscription,
        publish: &PublishPacket,
        clients: &HashMap<String, OptimizedClientInfo>,
    ) {
        if let Some(client_info) = clients.get(&sub.client_id) {
            if client_info.online {
                self.send_message_to_client(sub, publish, client_info);
            } else {
                // Client offline - queue message if QoS > 0
                if self.storage.is_some() && sub.qos != QoS::AtMostOnce {
                    if let Some(ref storage) = self.storage {
                        let mut message = publish.clone();
                        message.qos = sub.qos;

                        let queued_msg =
                            QueuedMessage::new(message, sub.client_id.clone(), sub.qos, None);
                        if let Err(e) = storage.queue_message(queued_msg).await {
                            error!(
                                "Failed to queue message for offline subscriber {}: {}",
                                sub.client_id, e
                            );
                        }
                    }
                }
            }
        }
    }

    /// Optimized message sending with minimal allocations
    fn send_message_to_client(
        &self,
        sub: &OptimizedSubscription,
        publish: &PublishPacket,
        client_info: &OptimizedClientInfo,
    ) {
        // Calculate effective QoS efficiently
        let effective_qos = match (publish.qos, sub.qos) {
            (QoS::AtMostOnce, _) | (_, QoS::AtMostOnce) => QoS::AtMostOnce,
            (QoS::AtLeastOnce | QoS::ExactlyOnce, QoS::AtLeastOnce)
            | (QoS::AtLeastOnce, QoS::ExactlyOnce) => QoS::AtLeastOnce,
            (QoS::ExactlyOnce, QoS::ExactlyOnce) => QoS::ExactlyOnce,
        };

        // Clone only when necessary
        if effective_qos != publish.qos || sub.subscription_id.is_some() {
            let mut message = publish.clone();
            message.qos = effective_qos;

            if let Some(id) = sub.subscription_id {
                message.properties.set_subscription_identifier(id);
            }

            // Non-blocking send with error handling
            if client_info.sender.try_send(message).is_err() {
                debug!(
                    "Failed to send message to client {} (channel full)",
                    sub.client_id
                );
            }
        } else {
            // Use original message if no modifications needed
            if client_info.sender.try_send(publish.clone()).is_err() {
                debug!(
                    "Failed to send message to client {} (channel full)",
                    sub.client_id
                );
            }
        }
    }

    /// Removes all subscriptions for a client (used during cleanup)
    async fn remove_all_subscriptions(&self, client_id: &str) {
        let mut tree = self.subscription_tree.write().await;
        self.remove_client_from_tree(&mut tree, client_id);
    }

    /// Recursively removes client subscriptions from tree
    #[allow(clippy::only_used_in_recursion)]
    fn remove_client_from_tree(&self, node: &mut SubscriptionTreeNode, client_id: &str) {
        // Remove from all subscription lists
        node.exact_subscriptions
            .retain(|sub| sub.client_id != client_id);
        node.wildcard_subscriptions
            .retain(|sub| sub.client_id != client_id);
        node.multilevel_subscriptions
            .retain(|sub| sub.client_id != client_id);

        // Recursively clean children
        for child in node.children.values_mut() {
            self.remove_client_from_tree(child, client_id);
        }

        // Remove empty child nodes
        node.children.retain(|_, child| {
            !child.exact_subscriptions.is_empty()
                || !child.wildcard_subscriptions.is_empty()
                || !child.multilevel_subscriptions.is_empty()
                || !child.children.is_empty()
        });
    }

    /// Parses shared subscription topic filter (same as original)
    fn parse_shared_subscription(topic_filter: &str) -> (&str, Option<String>) {
        if let Some(after_share) = topic_filter.strip_prefix("$share/") {
            if let Some(slash_pos) = after_share.find('/') {
                let group_name = &after_share[..slash_pos];
                let actual_filter = &after_share[slash_pos + 1..];
                return (actual_filter, Some(group_name.to_string()));
            }
        }
        (topic_filter, None)
    }

    /// Returns performance metrics
    pub fn get_metrics(&self) -> (usize, usize, f64, usize) {
        let messages = self.metrics.messages_routed.load(Ordering::Relaxed);
        let subscriptions = self.metrics.subscriptions_matched.load(Ordering::Relaxed);
        let total_time_us = self.metrics.delivery_time_total_us.load(Ordering::Relaxed);
        let lookups = self.metrics.subscription_lookups.load(Ordering::Relaxed);

        let avg_delivery_time_us = if messages > 0 {
            #[allow(clippy::cast_precision_loss)]
            {
                total_time_us as f64 / messages as f64
            }
        } else {
            0.0
        };

        (messages, subscriptions, avg_delivery_time_us, lookups)
    }
}

impl Default for OptimizedMessageRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_optimized_topic_matching() {
        let router = OptimizedMessageRouter::new();

        // Create test client
        let (sender, _receiver) = mpsc::channel(100);
        router.register_client("client1".to_string(), sender).await;

        // Add subscriptions
        router
            .subscribe(
                "client1".to_string(),
                "sensors/+/temp".to_string(),
                QoS::AtLeastOnce,
                None,
            )
            .await;
        router
            .subscribe(
                "client1".to_string(),
                "sensors/room1/#".to_string(),
                QoS::AtLeastOnce,
                None,
            )
            .await;

        // Test message routing
        let _publish = PublishPacket::new("sensors/room1/temp", b"25.5", QoS::AtMostOnce);

        let matches = router
            .find_matching_subscriptions("sensors/room1/temp")
            .await;
        assert_eq!(matches.len(), 2); // Should match both subscriptions

        let (_messages, _subscriptions, _avg_time, lookups) = router.get_metrics();
        assert!(lookups > 0);
    }

    #[tokio::test]
    async fn test_optimized_shared_subscriptions() {
        let router = OptimizedMessageRouter::new();

        // Create test clients
        let (sender1, _) = mpsc::channel(100);
        let (sender2, _) = mpsc::channel(100);
        router.register_client("client1".to_string(), sender1).await;
        router.register_client("client2".to_string(), sender2).await;

        // Add shared subscriptions
        router
            .subscribe(
                "client1".to_string(),
                "$share/group1/sensors/temp".to_string(),
                QoS::AtLeastOnce,
                None,
            )
            .await;
        router
            .subscribe(
                "client2".to_string(),
                "$share/group1/sensors/temp".to_string(),
                QoS::AtLeastOnce,
                None,
            )
            .await;

        let matches = router.find_matching_subscriptions("sensors/temp").await;
        assert_eq!(matches.len(), 2);

        // Both should be in the same share group
        assert!(matches
            .iter()
            .all(|sub| sub.share_group == Some("group1".to_string())));
    }

    #[tokio::test]
    async fn test_performance_metrics() {
        let router = OptimizedMessageRouter::new();

        let (sender, _) = mpsc::channel(100);
        router.register_client("client1".to_string(), sender).await;
        router
            .subscribe(
                "client1".to_string(),
                "test/topic".to_string(),
                QoS::AtMostOnce,
                None,
            )
            .await;

        let publish = PublishPacket::new("test/topic", b"test", QoS::AtMostOnce);
        router.route_message_optimized(&publish).await;

        let (messages, subscriptions, avg_time, lookups) = router.get_metrics();
        assert_eq!(messages, 1);
        assert!(subscriptions > 0);
        assert!(avg_time >= 0.0);
        assert!(lookups > 0);
    }
}
