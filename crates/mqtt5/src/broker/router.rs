//! Message routing for the MQTT broker
//!
//! Routes messages between clients based on subscriptions

#[cfg(not(target_arch = "wasm32"))]
use crate::broker::bridge::BridgeManager;
use crate::broker::storage::{DynamicStorage, QueuedMessage, RetainedMessage, StorageBackend};
use crate::packet::publish::PublishPacket;
use crate::validation::topic_matches_filter;
use crate::QoS;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Weak;
use tokio::sync::RwLock;
use tracing::{debug, error, info, trace};

#[cfg(target_arch = "wasm32")]
type WasmBridgeCallback = Box<dyn Fn(&PublishPacket)>;

/// Client subscription information
#[derive(Debug, Clone)]
pub struct Subscription {
    /// Client ID that owns this subscription
    pub client_id: String,
    /// Quality of Service level
    pub qos: QoS,
    /// Subscription identifier (MQTT v5.0)
    pub subscription_id: Option<u32>,
    /// Shared subscription group name (if this is a shared subscription)
    pub share_group: Option<String>,
    /// No Local option - if true, messages published by this client are not delivered back to it
    pub no_local: bool,
    /// Retain As Published option - if true, the retain flag is kept as-is when delivering
    pub retain_as_published: bool,
}

/// Message router for the broker
pub struct MessageRouter {
    subscriptions: Arc<RwLock<HashMap<String, Vec<Subscription>>>>,
    retained_messages: Arc<RwLock<HashMap<String, RetainedMessage>>>,
    clients: Arc<RwLock<HashMap<String, ClientInfo>>>,
    storage: Option<Arc<DynamicStorage>>,
    share_group_counters: Arc<RwLock<HashMap<String, Arc<AtomicUsize>>>>,
    #[cfg(not(target_arch = "wasm32"))]
    bridge_manager: Arc<RwLock<Option<Weak<BridgeManager>>>>,
    #[cfg(target_arch = "wasm32")]
    wasm_bridge_callback: Arc<RwLock<Option<WasmBridgeCallback>>>,
}

/// Information about a connected client
#[derive(Debug)]
pub struct ClientInfo {
    /// Channel to send messages to this client
    pub sender: tokio::sync::mpsc::Sender<PublishPacket>,
    /// Channel to signal disconnection (for session takeover)
    pub disconnect_tx: tokio::sync::oneshot::Sender<()>,
}

impl MessageRouter {
    /// Creates a new message router
    #[must_use]
    pub fn new() -> Self {
        Self {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            retained_messages: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            storage: None,
            share_group_counters: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(not(target_arch = "wasm32"))]
            bridge_manager: Arc::new(RwLock::new(None)),
            #[cfg(target_arch = "wasm32")]
            wasm_bridge_callback: Arc::new(RwLock::new(None)),
        }
    }

    /// Creates a new message router with storage backend
    #[must_use]
    pub fn with_storage(storage: Arc<DynamicStorage>) -> Self {
        Self {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            retained_messages: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            storage: Some(storage),
            share_group_counters: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(not(target_arch = "wasm32"))]
            bridge_manager: Arc::new(RwLock::new(None)),
            #[cfg(target_arch = "wasm32")]
            wasm_bridge_callback: Arc::new(RwLock::new(None)),
        }
    }

    /// Sets the bridge manager for this router
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn set_bridge_manager(&self, bridge_manager: Arc<BridgeManager>) {
        *self.bridge_manager.write().await = Some(Arc::downgrade(&bridge_manager));
    }

    /// Sets a WASM bridge callback that is called when messages need to be forwarded to bridges
    #[cfg(target_arch = "wasm32")]
    pub async fn set_wasm_bridge_callback<F>(&self, callback: F)
    where
        F: Fn(&PublishPacket) + 'static,
    {
        *self.wasm_bridge_callback.write().await = Some(Box::new(callback));
    }

    /// Initializes the router by loading retained messages from storage.
    ///
    /// # Errors
    /// Returns an error if the storage fails to load retained messages.
    pub async fn initialize(&self) -> Result<(), crate::error::MqttError> {
        if let Some(ref storage) = self.storage {
            // Load all retained messages from storage
            let stored_messages = storage.get_retained_messages("#").await?;
            let mut retained = self.retained_messages.write().await;

            for (topic, msg) in stored_messages {
                retained.insert(topic, msg);
            }

            debug!("Loaded {} retained messages from storage", retained.len());
        }
        Ok(())
    }

    /// Registers a client connection, signals old client to disconnect if ID already exists
    pub async fn register_client(
        &self,
        client_id: String,
        sender: tokio::sync::mpsc::Sender<PublishPacket>,
        new_disconnect_tx: tokio::sync::oneshot::Sender<()>,
    ) {
        let mut clients = self.clients.write().await;

        // Remove old client if exists and signal disconnect
        if let Some(old_client) = clients.remove(&client_id) {
            info!("Client ID takeover: {}", client_id);
            let _ = old_client.disconnect_tx.send(());
        }

        clients.insert(
            client_id.clone(),
            ClientInfo {
                sender,
                disconnect_tx: new_disconnect_tx,
            },
        );
        info!("Registered client: {}", client_id);
    }

    /// Disconnects a client but keeps subscriptions (for persistent sessions)
    pub async fn disconnect_client(&self, client_id: &str) {
        let mut clients = self.clients.write().await;
        clients.remove(client_id);
        debug!("Disconnected client (keeping subscriptions): {}", client_id);
    }

    /// Unregisters a client connection and removes all subscriptions
    pub async fn unregister_client(&self, client_id: &str) {
        let mut clients = self.clients.write().await;
        clients.remove(client_id);

        // Remove all subscriptions for this client
        let mut subscriptions = self.subscriptions.write().await;
        for subs in subscriptions.values_mut() {
            subs.retain(|sub| sub.client_id != client_id);
        }

        // Clean up empty subscription lists
        subscriptions.retain(|_, subs| !subs.is_empty());

        debug!("Unregistered client: {}", client_id);
    }

    pub async fn subscribe(
        &self,
        client_id: String,
        topic_filter: String,
        qos: QoS,
        subscription_id: Option<u32>,
        no_local: bool,
        retain_as_published: bool,
    ) -> bool {
        let (actual_filter, share_group) = Self::parse_shared_subscription(&topic_filter);

        let mut subscriptions = self.subscriptions.write().await;

        let subs = subscriptions.entry(actual_filter.to_string()).or_default();

        let existing_pos = subs.iter().position(|s| s.client_id == client_id);

        let subscription = Subscription {
            client_id: client_id.clone(),
            qos,
            subscription_id,
            share_group: share_group.clone(),
            no_local,
            retain_as_published,
        };

        let is_new = if let Some(pos) = existing_pos {
            subs[pos] = subscription;
            debug!(
                "Client {} updated subscription to {}",
                client_id, topic_filter
            );
            false
        } else {
            subs.push(subscription);
            debug!("Client {} subscribed to {}", client_id, topic_filter);
            true
        };

        if let Some(group) = share_group {
            let mut counters = self.share_group_counters.write().await;
            counters
                .entry(group)
                .or_insert_with(|| Arc::new(AtomicUsize::new(0)));
        }

        is_new
    }

    /// Parses a shared subscription topic filter
    /// Returns (actual_topic_filter, Option<share_group_name>)
    fn parse_shared_subscription(topic_filter: &str) -> (&str, Option<String>) {
        if let Some(after_share) = topic_filter.strip_prefix("$share/") {
            // Find the second '/' after $share/
            if let Some(slash_pos) = after_share.find('/') {
                let group_name = &after_share[..slash_pos];
                let actual_filter = &after_share[slash_pos + 1..];
                return (actual_filter, Some(group_name.to_string()));
            }
        }
        (topic_filter, None)
    }

    /// Removes a subscription for a client
    pub async fn unsubscribe(&self, client_id: &str, topic_filter: &str) -> bool {
        let (actual_filter, _) = Self::parse_shared_subscription(topic_filter);

        let mut subscriptions = self.subscriptions.write().await;

        if let Some(subs) = subscriptions.get_mut(actual_filter) {
            let initial_len = subs.len();
            subs.retain(|sub| sub.client_id != client_id);

            let removed = initial_len != subs.len();

            // Remove empty entries after calculating removed flag
            if subs.is_empty() {
                subscriptions.remove(actual_filter);
            }
            if removed {
                debug!("Client {} unsubscribed from {}", client_id, topic_filter);
            }
            removed
        } else {
            false
        }
    }

    /// Routes a publish message to all matching subscribers
    #[allow(clippy::too_many_lines)]
    pub async fn route_message(&self, publish: &PublishPacket, publishing_client_id: Option<&str>) {
        trace!("Routing message to topic: {}", publish.topic_name);

        // Handle retained messages
        if publish.retain {
            let mut retained = self.retained_messages.write().await;
            if publish.payload.is_empty() {
                // Empty payload means delete retained message
                retained.remove(&publish.topic_name);
                debug!("Deleted retained message for topic: {}", publish.topic_name);

                // Remove from storage
                if let Some(ref storage) = self.storage {
                    if let Err(e) = storage.remove_retained_message(&publish.topic_name).await {
                        tracing::error!("Failed to remove retained message from storage: {}", e);
                    }
                }
            } else {
                let retained_msg = RetainedMessage::new(publish.clone());
                retained.insert(publish.topic_name.clone(), retained_msg.clone());
                debug!("Stored retained message for topic: {}", publish.topic_name);

                if let Some(ref storage) = self.storage {
                    if let Err(e) = storage
                        .store_retained_message(&publish.topic_name, retained_msg)
                        .await
                    {
                        tracing::error!("Failed to store retained message to storage: {}", e);
                    }
                }
            }
        }

        // Find matching subscriptions
        let subscriptions = self.subscriptions.read().await;
        let clients = self.clients.read().await;

        // Group subscriptions by share group
        let mut share_groups: HashMap<String, Vec<&Subscription>> = HashMap::new();
        let mut regular_subs: Vec<&Subscription> = Vec::new();

        for (topic_filter, subs) in subscriptions.iter() {
            if topic_matches_filter(&publish.topic_name, topic_filter) {
                for sub in subs {
                    if let Some(ref group) = sub.share_group {
                        share_groups.entry(group.clone()).or_default().push(sub);
                    } else {
                        regular_subs.push(sub);
                    }
                }
            }
        }

        // Process shared subscriptions - one delivery per group
        for (group_name, group_subs) in share_groups {
            // Find online subscribers in this group
            let online_subs: Vec<&Subscription> = group_subs
                .iter()
                .filter(|sub| clients.contains_key(&sub.client_id))
                .copied()
                .collect();

            if !online_subs.is_empty() {
                // Get round-robin counter for this group
                let counters = self.share_group_counters.read().await;
                if let Some(counter) = counters.get(&group_name) {
                    let index = counter.fetch_add(1, Ordering::Relaxed) % online_subs.len();
                    let chosen_sub = online_subs[index];

                    // Deliver to chosen subscriber
                    self.deliver_to_subscriber(
                        chosen_sub,
                        publish,
                        &clients,
                        self.storage.as_ref(),
                        publishing_client_id,
                    )
                    .await;
                }
            } else if !group_subs.is_empty() {
                // All subscribers offline - queue for first subscriber if QoS > 0
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

        // Process regular (non-shared) subscriptions
        for sub in regular_subs {
            self.deliver_to_subscriber(
                sub,
                publish,
                &clients,
                self.storage.as_ref(),
                publishing_client_id,
            )
            .await;
        }

        // Forward to bridges if configured
        #[cfg(not(target_arch = "wasm32"))]
        {
            let bridge_manager_weak = self.bridge_manager.read().await.clone();
            if let Some(weak) = bridge_manager_weak {
                if let Some(bridge_manager) = weak.upgrade() {
                    if let Err(e) = bridge_manager.handle_outgoing(publish).await {
                        error!("Failed to forward message to bridges: {}", e);
                    }
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            let callback = self.wasm_bridge_callback.read().await;
            if let Some(ref cb) = *callback {
                cb(publish);
            }
        }
    }

    /// Routes a publish message to local subscribers only, without forwarding to bridges.
    ///
    /// This method is used by bridge connections to prevent message loops when receiving
    /// messages from remote brokers. It performs all local routing (retained messages,
    /// subscriptions, shared subscriptions) but skips bridge forwarding.
    ///
    /// # Arguments
    /// * `publish` - The publish packet to route
    /// * `publishing_client_id` - Optional client ID that published the message (used for no_local filtering)
    pub async fn route_message_local_only(
        &self,
        publish: &PublishPacket,
        publishing_client_id: Option<&str>,
    ) {
        trace!(
            "Routing message locally (no bridge) to topic: {}",
            publish.topic_name
        );

        if publish.retain {
            let mut retained = self.retained_messages.write().await;
            if publish.payload.is_empty() {
                retained.remove(&publish.topic_name);
                debug!("Deleted retained message for topic: {}", publish.topic_name);

                if let Some(ref storage) = self.storage {
                    if let Err(e) = storage.remove_retained_message(&publish.topic_name).await {
                        tracing::error!("Failed to remove retained message from storage: {}", e);
                    }
                }
            } else {
                let retained_msg = RetainedMessage::new(publish.clone());
                retained.insert(publish.topic_name.clone(), retained_msg.clone());
                debug!("Stored retained message for topic: {}", publish.topic_name);

                if let Some(ref storage) = self.storage {
                    if let Err(e) = storage
                        .store_retained_message(&publish.topic_name, retained_msg)
                        .await
                    {
                        tracing::error!("Failed to store retained message to storage: {}", e);
                    }
                }
            }
        }

        let subscriptions = self.subscriptions.read().await;
        let clients = self.clients.read().await;

        let mut share_groups: HashMap<String, Vec<&Subscription>> = HashMap::new();
        let mut regular_subs: Vec<&Subscription> = Vec::new();

        for (topic_filter, subs) in subscriptions.iter() {
            if topic_matches_filter(&publish.topic_name, topic_filter) {
                for sub in subs {
                    if let Some(ref group) = sub.share_group {
                        share_groups.entry(group.clone()).or_default().push(sub);
                    } else {
                        regular_subs.push(sub);
                    }
                }
            }
        }

        for (group_name, group_subs) in share_groups {
            let online_subs: Vec<&Subscription> = group_subs
                .iter()
                .filter(|sub| clients.contains_key(&sub.client_id))
                .copied()
                .collect();

            if !online_subs.is_empty() {
                let counters = self.share_group_counters.read().await;
                if let Some(counter) = counters.get(&group_name) {
                    let index = counter.fetch_add(1, Ordering::Relaxed) % online_subs.len();
                    let chosen_sub = online_subs[index];

                    self.deliver_to_subscriber(
                        chosen_sub,
                        publish,
                        &clients,
                        self.storage.as_ref(),
                        publishing_client_id,
                    )
                    .await;
                }
            } else if !group_subs.is_empty() {
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

        for sub in regular_subs {
            self.deliver_to_subscriber(
                sub,
                publish,
                &clients,
                self.storage.as_ref(),
                publishing_client_id,
            )
            .await;
        }
    }

    /// Delivers a message to a specific subscriber
    async fn deliver_to_subscriber(
        &self,
        sub: &Subscription,
        publish: &PublishPacket,
        clients: &HashMap<String, ClientInfo>,
        storage: Option<&Arc<DynamicStorage>>,
        publishing_client_id: Option<&str>,
    ) {
        if sub.no_local {
            if let Some(publisher_id) = publishing_client_id {
                if publisher_id == sub.client_id {
                    trace!(
                        "Skipping delivery to {} due to No Local flag",
                        sub.client_id
                    );
                    return;
                }
            }
        }

        if let Some(client_info) = clients.get(&sub.client_id) {
            // Calculate effective QoS (minimum of publish and subscription QoS)
            let effective_qos = match (publish.qos, sub.qos) {
                (QoS::AtMostOnce, _) | (_, QoS::AtMostOnce) => QoS::AtMostOnce,
                (QoS::AtLeastOnce | QoS::ExactlyOnce, QoS::AtLeastOnce)
                | (QoS::AtLeastOnce, QoS::ExactlyOnce) => QoS::AtLeastOnce,
                (QoS::ExactlyOnce, QoS::ExactlyOnce) => QoS::ExactlyOnce,
            };

            let mut message = publish.clone();
            message.qos = effective_qos;

            if !sub.retain_as_published {
                message.retain = false;
            }

            if let Some(id) = sub.subscription_id {
                message.properties.set_subscription_identifier(id);
            }

            // Send to client (non-blocking)
            if let Err(e) = client_info.sender.try_send(message.clone()) {
                // Client's channel is full or disconnected, queue for later
                if let Some(storage) = storage {
                    if effective_qos != QoS::AtMostOnce {
                        let queued_msg = QueuedMessage::new(
                            message.clone(),
                            sub.client_id.clone(),
                            effective_qos,
                            message.packet_id,
                        );
                        if let Err(e) = storage.queue_message(queued_msg).await {
                            error!(
                                "Failed to queue message for offline client {}: {}",
                                sub.client_id, e
                            );
                        } else {
                            debug!("Queued message for client {}", sub.client_id);
                        }
                    }
                }
                trace!(
                    "Failed to send message to client {}: {:?}",
                    sub.client_id,
                    e
                );
            }
        } else {
            // Client is offline, queue message if QoS > 0
            if let Some(storage) = storage {
                if sub.qos != QoS::AtMostOnce {
                    let mut message = publish.clone();
                    message.qos = sub.qos;

                    let queued_msg = QueuedMessage::new(
                        message,
                        sub.client_id.clone(),
                        sub.qos,
                        None, // Will be assigned when delivered
                    );
                    if let Err(e) = storage.queue_message(queued_msg).await {
                        error!(
                            "Failed to queue message for offline client {}: {}",
                            sub.client_id, e
                        );
                    } else {
                        info!(
                            "Queued message for offline client {} on topic {}",
                            sub.client_id, publish.topic_name
                        );
                    }
                }
            } else {
                debug!(
                    "No storage configured, cannot queue message for offline client {}",
                    sub.client_id
                );
            }
        }
    }

    /// Gets retained messages matching a topic filter
    pub async fn get_retained_messages(&self, topic_filter: &str) -> Vec<PublishPacket> {
        let retained = self.retained_messages.read().await;
        retained
            .iter()
            .filter(|(topic, msg)| topic_matches_filter(topic, topic_filter) && !msg.is_expired())
            .map(|(_, msg)| msg.to_publish_packet())
            .collect()
    }

    /// Gets the number of connected clients
    pub async fn client_count(&self) -> usize {
        self.clients.read().await.len()
    }

    /// Gets the number of unique topic filters with subscriptions
    pub async fn topic_count(&self) -> usize {
        self.subscriptions.read().await.len()
    }

    /// Gets the number of retained messages
    pub async fn retained_count(&self) -> usize {
        self.retained_messages.read().await.len()
    }
}

impl Default for MessageRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_client_registration() {
        let router = MessageRouter::new();
        let (tx, _rx) = tokio::sync::mpsc::channel(100);

        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router.register_client("client1".to_string(), tx, dtx).await;
        assert_eq!(router.client_count().await, 1);

        router.unregister_client("client1").await;
        assert_eq!(router.client_count().await, 0);
    }

    #[tokio::test]
    async fn test_subscription_management() {
        let router = MessageRouter::new();
        let (tx, _rx) = tokio::sync::mpsc::channel(100);

        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router.register_client("client1".to_string(), tx, dtx).await;
        router
            .subscribe(
                "client1".to_string(),
                "test/+".to_string(),
                QoS::AtLeastOnce,
                None,
                false,
                false,
            )
            .await;

        assert_eq!(router.topic_count().await, 1);

        let removed = router.unsubscribe("client1", "test/+").await;
        assert!(removed);
        assert_eq!(router.topic_count().await, 0);
    }

    #[tokio::test]
    async fn test_message_routing() {
        let router = MessageRouter::new();
        let (tx1, mut rx1) = tokio::sync::mpsc::channel(100);
        let (tx2, mut rx2) = tokio::sync::mpsc::channel(100);

        // Register clients
        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client("client1".to_string(), tx1, dtx1)
            .await;
        router
            .register_client("client2".to_string(), tx2, dtx2)
            .await;

        router
            .subscribe(
                "client1".to_string(),
                "test/+".to_string(),
                QoS::AtLeastOnce,
                None,
                false,
                false,
            )
            .await;
        router
            .subscribe(
                "client2".to_string(),
                "test/data".to_string(),
                QoS::ExactlyOnce,
                None,
                false,
                false,
            )
            .await;

        // Publish message
        let publish = PublishPacket::new("test/data", b"hello", QoS::ExactlyOnce);

        router.route_message(&publish, None).await;

        // Client 1 should receive with QoS 1 (downgraded)
        let msg1 = rx1.try_recv().unwrap();
        assert_eq!(msg1.topic_name, "test/data");
        assert_eq!(msg1.qos, QoS::AtLeastOnce);

        // Client 2 should receive with QoS 2
        let msg2 = rx2.try_recv().unwrap();
        assert_eq!(msg2.topic_name, "test/data");
        assert_eq!(msg2.qos, QoS::ExactlyOnce);
    }

    #[tokio::test]
    async fn test_retained_messages() {
        let router = MessageRouter::new();

        // Store retained message
        let mut publish = PublishPacket::new("test/status", b"online", QoS::AtMostOnce);
        publish.retain = true;
        router.route_message(&publish, None).await;

        assert_eq!(router.retained_count().await, 1);

        // Get retained messages
        let retained = router.get_retained_messages("test/+").await;
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].topic_name, "test/status");

        // Delete retained message
        let mut delete = PublishPacket::new("test/status", b"", QoS::AtMostOnce);
        delete.retain = true;
        router.route_message(&delete, None).await;

        assert_eq!(router.retained_count().await, 0);
    }

    #[tokio::test]
    async fn test_shared_subscription_parsing() {
        let (filter, group) = MessageRouter::parse_shared_subscription("$share/group1/test/topic");
        assert_eq!(filter, "test/topic");
        assert_eq!(group, Some("group1".to_string()));

        let (filter, group) = MessageRouter::parse_shared_subscription("test/topic");
        assert_eq!(filter, "test/topic");
        assert_eq!(group, None);

        let (filter, group) = MessageRouter::parse_shared_subscription("$share/group/test/+/data");
        assert_eq!(filter, "test/+/data");
        assert_eq!(group, Some("group".to_string()));
    }

    #[tokio::test]
    async fn test_shared_subscription_round_robin() {
        let router = MessageRouter::new();
        let (tx1, mut rx1) = tokio::sync::mpsc::channel(100);
        let (tx2, mut rx2) = tokio::sync::mpsc::channel(100);
        let (tx3, mut rx3) = tokio::sync::mpsc::channel(100);

        // Register three clients
        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client("client1".to_string(), tx1, dtx1)
            .await;
        router
            .register_client("client2".to_string(), tx2, dtx2)
            .await;
        let (dtx3, _drx3) = tokio::sync::oneshot::channel();
        router
            .register_client("client3".to_string(), tx3, dtx3)
            .await;

        router
            .subscribe(
                "client1".to_string(),
                "$share/workers/test/data".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
            )
            .await;
        router
            .subscribe(
                "client2".to_string(),
                "$share/workers/test/data".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
            )
            .await;
        router
            .subscribe(
                "client3".to_string(),
                "$share/workers/test/data".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
            )
            .await;

        // Publish 6 messages
        for i in 0..6 {
            let publish =
                PublishPacket::new("test/data", format!("msg{}", i).as_bytes(), QoS::AtMostOnce);
            router.route_message(&publish, None).await;
        }

        // Each client should receive exactly 2 messages
        let mut count1 = 0;
        let mut count2 = 0;
        let mut count3 = 0;

        while rx1.try_recv().is_ok() {
            count1 += 1;
        }
        while rx2.try_recv().is_ok() {
            count2 += 1;
        }
        while rx3.try_recv().is_ok() {
            count3 += 1;
        }

        assert_eq!(count1, 2);
        assert_eq!(count2, 2);
        assert_eq!(count3, 2);
    }

    #[tokio::test]
    async fn test_shared_and_regular_subscriptions() {
        let router = MessageRouter::new();
        let (tx1, mut rx1) = tokio::sync::mpsc::channel(100);
        let (tx2, mut rx2) = tokio::sync::mpsc::channel(100);
        let (tx3, mut rx3) = tokio::sync::mpsc::channel(100);

        // Register clients
        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        router
            .register_client("shared1".to_string(), tx1, dtx1)
            .await;
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client("shared2".to_string(), tx2, dtx2)
            .await;
        let (dtx3, _drx3) = tokio::sync::oneshot::channel();
        router
            .register_client("regular".to_string(), tx3, dtx3)
            .await;

        router
            .subscribe(
                "shared1".to_string(),
                "$share/group/test/+".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
            )
            .await;
        router
            .subscribe(
                "shared2".to_string(),
                "$share/group/test/+".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
            )
            .await;

        router
            .subscribe(
                "regular".to_string(),
                "test/+".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
            )
            .await;

        // Publish message
        let publish = PublishPacket::new("test/data", b"hello", QoS::AtMostOnce);
        router.route_message(&publish, None).await;

        // Regular subscriber should receive the message
        let regular_msg = rx3.try_recv().unwrap();
        assert_eq!(regular_msg.payload, b"hello");

        // Only one of the shared subscribers should receive it
        let shared1_received = rx1.try_recv().is_ok();
        let shared2_received = rx2.try_recv().is_ok();

        assert!(shared1_received ^ shared2_received); // XOR - exactly one should be true
    }
}
