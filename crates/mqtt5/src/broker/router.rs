//! Message routing for the MQTT broker
//!
//! Routes messages between clients based on subscriptions

#[cfg(not(target_arch = "wasm32"))]
use crate::broker::bridge::BridgeManager;
use crate::broker::events::{BrokerEventHandler, RetainedSetEvent};
use crate::broker::storage::{DynamicStorage, QueuedMessage, RetainedMessage, StorageBackend};
use crate::packet::publish::PublishPacket;
use crate::types::ProtocolVersion;
use crate::validation::{parse_shared_subscription, topic_matches_filter};
use crate::QoS;
use crate::Result;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Weak;
use tokio::sync::RwLock;
use tracing::{debug, error, info, trace, warn};

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
    /// Retain handling option - controls when retained messages are sent
    pub retain_handling: u8,
    /// Protocol version of the subscriber
    pub protocol_version: ProtocolVersion,
}

/// Message router for the broker
pub struct MessageRouter {
    exact_subscriptions: Arc<RwLock<HashMap<String, Vec<Subscription>>>>,
    wildcard_subscriptions: Arc<RwLock<HashMap<String, Vec<Subscription>>>>,
    retained_messages: Arc<RwLock<HashMap<String, RetainedMessage>>>,
    clients: Arc<RwLock<HashMap<String, ClientInfo>>>,
    storage: Option<Arc<DynamicStorage>>,
    share_group_counters: Arc<RwLock<HashMap<String, Arc<AtomicUsize>>>>,
    event_handler: Option<Arc<dyn BrokerEventHandler>>,
    #[cfg(not(target_arch = "wasm32"))]
    bridge_manager: Arc<RwLock<Option<Weak<BridgeManager>>>>,
    #[cfg(target_arch = "wasm32")]
    wasm_bridge_callback: Arc<RwLock<Option<WasmBridgeCallback>>>,
}

/// Information about a connected client
#[derive(Debug)]
pub struct ClientInfo {
    /// Channel to send messages to this client
    pub sender: flume::Sender<PublishPacket>,
    /// Channel to signal disconnection (for session takeover)
    pub disconnect_tx: tokio::sync::oneshot::Sender<()>,
}

impl MessageRouter {
    /// Creates a new message router
    #[must_use]
    pub fn new() -> Self {
        Self {
            exact_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            wildcard_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            retained_messages: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            storage: None,
            share_group_counters: Arc::new(RwLock::new(HashMap::new())),
            event_handler: None,
            #[cfg(not(target_arch = "wasm32"))]
            bridge_manager: Arc::new(RwLock::new(None)),
            #[cfg(target_arch = "wasm32")]
            #[allow(clippy::arc_with_non_send_sync)]
            wasm_bridge_callback: Arc::new(RwLock::new(None)),
        }
    }

    /// Creates a new message router with storage backend
    #[must_use]
    pub fn with_storage(storage: Arc<DynamicStorage>) -> Self {
        Self {
            exact_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            wildcard_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            retained_messages: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            storage: Some(storage),
            share_group_counters: Arc::new(RwLock::new(HashMap::new())),
            event_handler: None,
            #[cfg(not(target_arch = "wasm32"))]
            bridge_manager: Arc::new(RwLock::new(None)),
            #[cfg(target_arch = "wasm32")]
            #[allow(clippy::arc_with_non_send_sync)]
            wasm_bridge_callback: Arc::new(RwLock::new(None)),
        }
    }

    #[must_use]
    pub fn with_event_handler(mut self, handler: Arc<dyn BrokerEventHandler>) -> Self {
        self.event_handler = Some(handler);
        self
    }

    fn has_wildcards(topic_filter: &str) -> bool {
        topic_filter.contains('+') || topic_filter.contains('#')
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
    pub async fn initialize(&self) -> Result<()> {
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
        sender: flume::Sender<PublishPacket>,
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

        {
            let mut exact = self.exact_subscriptions.write().await;
            for subs in exact.values_mut() {
                subs.retain(|sub| sub.client_id != client_id);
            }
            exact.retain(|_, subs| !subs.is_empty());
        }

        {
            let mut wildcard = self.wildcard_subscriptions.write().await;
            for subs in wildcard.values_mut() {
                subs.retain(|sub| sub.client_id != client_id);
            }
            wildcard.retain(|_, subs| !subs.is_empty());
        }

        debug!("Unregistered client: {}", client_id);
    }

    /// Adds a subscription for a client.
    ///
    /// # Errors
    /// Returns an error if subscription registration fails or `retain_handling` is invalid.
    #[allow(clippy::too_many_arguments)]
    pub async fn subscribe(
        &self,
        client_id: String,
        topic_filter: String,
        qos: QoS,
        subscription_id: Option<u32>,
        no_local: bool,
        retain_as_published: bool,
        retain_handling: u8,
        protocol_version: ProtocolVersion,
    ) -> Result<bool> {
        if retain_handling > 2 {
            return Err(crate::MqttError::ProtocolError(format!(
                "Invalid retain_handling value: {retain_handling} (must be 0, 1, or 2)"
            )));
        }

        let (actual_filter, share_group) = parse_shared_subscription(&topic_filter);
        let share_group = share_group.map(str::to_string);

        let subscription = Subscription {
            client_id: client_id.clone(),
            qos,
            subscription_id,
            share_group: share_group.clone(),
            no_local,
            retain_as_published,
            retain_handling,
            protocol_version,
        };

        let is_new = if Self::has_wildcards(actual_filter) {
            let mut wildcard = self.wildcard_subscriptions.write().await;
            let subs = wildcard.entry(actual_filter.to_string()).or_default();
            let existing_pos = subs.iter().position(|s| s.client_id == client_id);
            if let Some(pos) = existing_pos {
                subs[pos] = subscription;
                debug!(
                    "Client {} updated wildcard subscription to {}",
                    client_id, topic_filter
                );
                false
            } else {
                subs.push(subscription);
                debug!(
                    "Client {} subscribed to wildcard {}",
                    client_id, topic_filter
                );
                true
            }
        } else {
            let mut exact = self.exact_subscriptions.write().await;
            let subs = exact.entry(actual_filter.to_string()).or_default();
            let existing_pos = subs.iter().position(|s| s.client_id == client_id);
            if let Some(pos) = existing_pos {
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
            }
        };

        if let Some(group) = share_group {
            let mut counters = self.share_group_counters.write().await;
            counters
                .entry(group)
                .or_insert_with(|| Arc::new(AtomicUsize::new(0)));
        }

        Ok(is_new)
    }

    /// Removes a subscription for a client
    pub async fn unsubscribe(&self, client_id: &str, topic_filter: &str) -> bool {
        let (actual_filter, _) = parse_shared_subscription(topic_filter);

        let subscriptions = if Self::has_wildcards(actual_filter) {
            &self.wildcard_subscriptions
        } else {
            &self.exact_subscriptions
        };

        let mut subs_map = subscriptions.write().await;

        if let Some(subs) = subs_map.get_mut(actual_filter) {
            let initial_len = subs.len();
            subs.retain(|sub| sub.client_id != client_id);

            let removed = initial_len != subs.len();

            if subs.is_empty() {
                subs_map.remove(actual_filter);
            }
            if removed {
                debug!("Client {} unsubscribed from {}", client_id, topic_filter);
            }
            removed
        } else {
            false
        }
    }

    /// Routes a publish message to all matching subscribers and forwards to bridges
    pub async fn route_message(&self, publish: &PublishPacket, publishing_client_id: Option<&str>) {
        self.route_message_internal(publish, publishing_client_id, true)
            .await;
    }

    /// Routes a publish message to local subscribers only, without forwarding to bridges.
    ///
    /// This method is used by bridge connections to prevent message loops when receiving
    /// messages from remote brokers. It performs all local routing (retained messages,
    /// subscriptions, shared subscriptions) but skips bridge forwarding.
    ///
    /// # Arguments
    /// * `publish` - The publish packet to route
    /// * `publishing_client_id` - Optional client ID that published the message (used for `no_local` filtering)
    pub async fn route_message_local_only(
        &self,
        publish: &PublishPacket,
        publishing_client_id: Option<&str>,
    ) {
        self.route_message_internal(publish, publishing_client_id, false)
            .await;
    }

    #[allow(clippy::too_many_lines)]
    async fn route_message_internal(
        &self,
        publish: &PublishPacket,
        publishing_client_id: Option<&str>,
        forward_to_bridges: bool,
    ) {
        if forward_to_bridges {
            trace!("Routing message to topic: {}", publish.topic_name);
        } else {
            trace!(
                "Routing message locally (no bridge) to topic: {}",
                publish.topic_name
            );
        }

        if publish.retain {
            let mut retained = self.retained_messages.write().await;
            if publish.payload.is_empty() {
                retained.remove(&publish.topic_name);
                debug!("Deleted retained message for topic: {}", publish.topic_name);

                if let Some(ref storage) = self.storage {
                    if let Err(e) = storage.remove_retained_message(&publish.topic_name).await {
                        tracing::error!("Failed to remove retained message from storage: {e}");
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
                        tracing::error!("Failed to store retained message to storage: {e}");
                    }
                }
            }

            if let Some(ref handler) = self.event_handler {
                let event = RetainedSetEvent {
                    topic: Arc::from(publish.topic_name.as_str()),
                    payload: publish.payload.clone(),
                    qos: publish.qos,
                    cleared: publish.payload.is_empty(),
                };
                handler.on_retained_set(event).await;
            }
        }

        let exact = self.exact_subscriptions.read().await;
        let wildcard = self.wildcard_subscriptions.read().await;
        let clients = self.clients.read().await;

        let mut share_groups: HashMap<String, Vec<&Subscription>> = HashMap::new();
        let mut regular_subs: Vec<&Subscription> = Vec::new();

        if let Some(subs) = exact.get(&publish.topic_name) {
            for sub in subs {
                if let Some(ref group) = sub.share_group {
                    share_groups.entry(group.clone()).or_default().push(sub);
                } else {
                    regular_subs.push(sub);
                }
            }
        }

        for (topic_filter, subs) in wildcard.iter() {
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

        if forward_to_bridges {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let bridge_manager_weak = self.bridge_manager.read().await.clone();
                if let Some(weak) = bridge_manager_weak {
                    if let Some(bridge_manager) = weak.upgrade() {
                        if let Err(e) = bridge_manager.handle_outgoing(publish).await {
                            error!("Failed to forward message to bridges: {e}");
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
    }

    /// Delivers a message to a specific subscriber
    #[allow(clippy::too_many_lines)]
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
            let effective_qos = match (publish.qos, sub.qos) {
                (QoS::AtMostOnce, _) | (_, QoS::AtMostOnce) => QoS::AtMostOnce,
                (QoS::AtLeastOnce | QoS::ExactlyOnce, QoS::AtLeastOnce)
                | (QoS::AtLeastOnce, QoS::ExactlyOnce) => QoS::AtLeastOnce,
                (QoS::ExactlyOnce, QoS::ExactlyOnce) => QoS::ExactlyOnce,
            };

            let mut message = publish.clone();
            message.qos = effective_qos;
            if publish.protocol_version == 5
                && sub.protocol_version == ProtocolVersion::V311
                && !publish.properties.is_empty()
            {
                trace!(
                    topic = %publish.topic_name,
                    client = %sub.client_id,
                    "Stripping v5 properties for v3.1.1 subscriber"
                );
            }
            message.protocol_version = sub.protocol_version.as_u8();

            if !sub.retain_as_published {
                message.retain = false;
            }

            if let Some(id) = sub.subscription_id {
                message.properties.set_subscription_identifier(id);
            }

            match client_info.sender.try_send(message) {
                Ok(()) => {}
                Err(e) => {
                    warn!(
                        client_id = %sub.client_id,
                        topic = %publish.topic_name,
                        "Channel send failed - message may be dropped"
                    );
                    let message = e.into_inner();
                    if let Some(storage) = storage {
                        if effective_qos != QoS::AtMostOnce {
                            let queued_msg = QueuedMessage::new(
                                message,
                                sub.client_id.clone(),
                                effective_qos,
                                None,
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
                }
            }
        } else if let Some(storage) = storage {
            if sub.qos != QoS::AtMostOnce {
                let mut message = publish.clone();
                message.qos = sub.qos;
                if publish.protocol_version == 5
                    && sub.protocol_version == ProtocolVersion::V311
                    && !publish.properties.is_empty()
                {
                    trace!(
                        topic = %publish.topic_name,
                        client = %sub.client_id,
                        "Stripping v5 properties for queued message to v3.1.1 subscriber"
                    );
                }
                message.protocol_version = sub.protocol_version.as_u8();

                let queued_msg = QueuedMessage::new(message, sub.client_id.clone(), sub.qos, None);
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
        let exact = self.exact_subscriptions.read().await;
        let wildcard = self.wildcard_subscriptions.read().await;
        exact.len() + wildcard.len()
    }

    /// Gets the number of retained messages
    pub async fn retained_count(&self) -> usize {
        self.retained_messages.read().await.len()
    }

    /// Gets the number of subscriptions for a specific client
    pub async fn subscription_count_for_client(&self, client_id: &str) -> usize {
        let exact = self.exact_subscriptions.read().await;
        let wildcard = self.wildcard_subscriptions.read().await;
        let exact_count = exact
            .values()
            .flat_map(|subs| subs.iter())
            .filter(|sub| sub.client_id == client_id)
            .count();
        let wildcard_count = wildcard
            .values()
            .flat_map(|subs| subs.iter())
            .filter(|sub| sub.client_id == client_id)
            .count();
        exact_count + wildcard_count
    }

    pub async fn has_subscription(&self, client_id: &str, topic_filter: &str) -> bool {
        let (actual_filter, _) = parse_shared_subscription(topic_filter);
        let subscriptions = if Self::has_wildcards(actual_filter) {
            &self.wildcard_subscriptions
        } else {
            &self.exact_subscriptions
        };
        let subs_map = subscriptions.read().await;
        subs_map
            .get(actual_filter)
            .is_some_and(|subs| subs.iter().any(|sub| sub.client_id == client_id))
    }

    /// Checks if a retained message already exists for a topic
    pub async fn has_retained_message(&self, topic: &str) -> bool {
        let retained = self.retained_messages.read().await;
        retained.contains_key(topic)
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
    use bytes::Bytes;

    #[tokio::test]
    async fn test_client_registration() {
        let router = MessageRouter::new();
        let (tx, _rx) = flume::bounded(100);

        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router.register_client("client1".to_string(), tx, dtx).await;
        assert_eq!(router.client_count().await, 1);

        router.unregister_client("client1").await;
        assert_eq!(router.client_count().await, 0);
    }

    #[tokio::test]
    async fn test_subscription_management() {
        let router = MessageRouter::new();
        let (tx, _rx) = flume::bounded(100);

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
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        assert_eq!(router.topic_count().await, 1);

        let removed = router.unsubscribe("client1", "test/+").await;
        assert!(removed);
        assert_eq!(router.topic_count().await, 0);
    }

    #[tokio::test]
    async fn test_message_routing() {
        let router = MessageRouter::new();
        let (tx1, rx1) = flume::bounded(100);
        let (tx2, rx2) = flume::bounded(100);

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
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();
        router
            .subscribe(
                "client2".to_string(),
                "test/data".to_string(),
                QoS::ExactlyOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Publish message
        let publish = PublishPacket::new("test/data", &b"hello"[..], QoS::ExactlyOnce);

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
        let mut publish = PublishPacket::new("test/status", &b"online"[..], QoS::AtMostOnce);
        publish.retain = true;
        router.route_message(&publish, None).await;

        assert_eq!(router.retained_count().await, 1);

        // Get retained messages
        let retained = router.get_retained_messages("test/+").await;
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].topic_name, "test/status");

        // Delete retained message
        let mut delete = PublishPacket::new("test/status", &b""[..], QoS::AtMostOnce);
        delete.retain = true;
        router.route_message(&delete, None).await;

        assert_eq!(router.retained_count().await, 0);
    }

    #[tokio::test]
    async fn test_shared_subscription_round_robin() {
        let router = MessageRouter::new();
        let (tx1, rx1) = flume::bounded(100);
        let (tx2, rx2) = flume::bounded(100);
        let (tx3, rx3) = flume::bounded(100);

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
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();
        router
            .subscribe(
                "client2".to_string(),
                "$share/workers/test/data".to_string(),
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
                "client3".to_string(),
                "$share/workers/test/data".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Publish 6 messages
        for i in 0..6 {
            let publish = PublishPacket::new(
                "test/data",
                Bytes::copy_from_slice(format!("msg{i}").as_bytes()),
                QoS::AtMostOnce,
            );
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
        let (tx1, rx1) = flume::bounded(100);
        let (tx2, rx2) = flume::bounded(100);
        let (tx3, rx3) = flume::bounded(100);

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
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();
        router
            .subscribe(
                "shared2".to_string(),
                "$share/group/test/+".to_string(),
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
                "regular".to_string(),
                "test/+".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
            )
            .await
            .unwrap();

        // Publish message
        let publish = PublishPacket::new("test/data", &b"hello"[..], QoS::AtMostOnce);
        router.route_message(&publish, None).await;

        // Regular subscriber should receive the message
        let regular_msg = rx3.try_recv().unwrap();
        assert_eq!(&regular_msg.payload[..], b"hello");

        // Only one of the shared subscribers should receive it
        let shared1_received = rx1.try_recv().is_ok();
        let shared2_received = rx2.try_recv().is_ok();

        assert!(shared1_received ^ shared2_received); // XOR - exactly one should be true
    }
}
