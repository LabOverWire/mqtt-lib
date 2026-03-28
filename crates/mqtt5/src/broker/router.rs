#[cfg(not(target_arch = "wasm32"))]
use crate::broker::bridge::BridgeManager;
use crate::broker::events::{BrokerEventHandler, RetainedSetEvent};
use crate::broker::storage::{
    ChangeOnlyState, DynamicStorage, QueuedMessage, RetainedMessage, StorageBackend,
};
use crate::packet::publish::PublishPacket;
use crate::types::ProtocolVersion;
use crate::validation::{parse_shared_subscription, topic_matches_filter};
use crate::QoS;
use crate::Result;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Weak;
use tokio::sync::RwLock;
use tracing::{debug, error, info, trace, warn};

struct OutboundRateState {
    count: AtomicU32,
    window_start: parking_lot::Mutex<crate::time::Instant>,
}

impl OutboundRateState {
    fn new() -> Self {
        Self {
            count: AtomicU32::new(0),
            window_start: parking_lot::Mutex::new(crate::time::Instant::now()),
        }
    }

    fn check_and_increment(&self, max_rate: u32) -> bool {
        if max_rate == 0 {
            return true;
        }

        let mut window_start = self.window_start.lock();
        if window_start.elapsed() >= crate::time::Duration::from_secs(1) {
            *window_start = crate::time::Instant::now();
            self.count.store(1, Ordering::Relaxed);
            return true;
        }
        drop(window_start);

        let prev = self.count.fetch_add(1, Ordering::Relaxed);
        if prev >= max_rate {
            self.count.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }
}

#[cfg(target_arch = "wasm32")]
type WasmBridgeCallback = Box<dyn Fn(&PublishPacket)>;

pub struct RoutableMessage {
    pub publish: PublishPacket,
    pub target_flow: Option<u64>,
}

/// Client subscription information
#[derive(Debug, Clone)]
pub struct Subscription {
    pub client_id: String,
    pub qos: QoS,
    pub subscription_id: Option<u32>,
    pub share_group: Option<String>,
    pub no_local: bool,
    pub retain_as_published: bool,
    pub retain_handling: u8,
    pub protocol_version: ProtocolVersion,
    pub change_only: bool,
    pub flow_id: Option<u64>,
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
    change_only_states: Arc<RwLock<HashMap<String, ChangeOnlyState>>>,
    #[cfg(not(target_arch = "wasm32"))]
    bridge_manager: Arc<RwLock<Option<Weak<BridgeManager>>>>,
    #[cfg(target_arch = "wasm32")]
    wasm_bridge_callback: Arc<RwLock<Option<WasmBridgeCallback>>>,
    echo_suppression_key: Arc<RwLock<Option<String>>>,
    outbound_rates: parking_lot::RwLock<HashMap<String, OutboundRateState>>,
    max_outbound_rate: AtomicU32,
}

/// Information about a connected client
#[derive(Debug)]
pub struct ClientInfo {
    pub sender: flume::Sender<RoutableMessage>,
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
            change_only_states: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(not(target_arch = "wasm32"))]
            bridge_manager: Arc::new(RwLock::new(None)),
            #[cfg(target_arch = "wasm32")]
            #[allow(clippy::arc_with_non_send_sync)]
            wasm_bridge_callback: Arc::new(RwLock::new(None)),
            echo_suppression_key: Arc::new(RwLock::new(None)),
            outbound_rates: parking_lot::RwLock::new(HashMap::new()),
            max_outbound_rate: AtomicU32::new(0),
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
            change_only_states: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(not(target_arch = "wasm32"))]
            bridge_manager: Arc::new(RwLock::new(None)),
            #[cfg(target_arch = "wasm32")]
            #[allow(clippy::arc_with_non_send_sync)]
            wasm_bridge_callback: Arc::new(RwLock::new(None)),
            echo_suppression_key: Arc::new(RwLock::new(None)),
            outbound_rates: parking_lot::RwLock::new(HashMap::new()),
            max_outbound_rate: AtomicU32::new(0),
        }
    }

    #[must_use]
    pub fn with_event_handler(mut self, handler: Arc<dyn BrokerEventHandler>) -> Self {
        self.event_handler = Some(handler);
        self
    }

    #[must_use]
    pub fn with_echo_suppression_key(mut self, key: String) -> Self {
        self.echo_suppression_key = Arc::new(RwLock::new(Some(key)));
        self
    }

    pub async fn update_echo_suppression_key(&self, key: Option<String>) {
        *self.echo_suppression_key.write().await = key;
    }

    #[must_use]
    pub fn try_update_echo_suppression_key(&self, key: Option<String>) -> bool {
        match self.echo_suppression_key.try_write() {
            Ok(mut guard) => {
                *guard = key;
                true
            }
            Err(_) => false,
        }
    }

    #[must_use]
    pub fn with_max_outbound_rate(self, rate: u32) -> Self {
        self.max_outbound_rate.store(rate, Ordering::Relaxed);
        self
    }

    pub fn update_max_outbound_rate(&self, rate: u32) {
        self.max_outbound_rate.store(rate, Ordering::Relaxed);
    }

    fn has_wildcards(topic_filter: &str) -> bool {
        topic_filter.contains('+') || topic_filter.contains('#')
    }

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
            let stored_messages = storage.get_retained_messages("#").await?;
            let mut retained = self.retained_messages.write().await;

            for (topic, msg) in stored_messages {
                retained.insert(topic, msg);
            }

            debug!("Loaded {} retained messages from storage", retained.len());
        }
        Ok(())
    }

    pub async fn register_client(
        &self,
        client_id: String,
        sender: flume::Sender<RoutableMessage>,
        new_disconnect_tx: tokio::sync::oneshot::Sender<()>,
    ) {
        let mut clients = self.clients.write().await;

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
        self.outbound_rates
            .write()
            .insert(client_id.clone(), OutboundRateState::new());
        info!("Registered client: {}", client_id);
    }

    pub async fn disconnect_client(&self, client_id: &str) {
        let mut clients = self.clients.write().await;
        clients.remove(client_id);
        self.outbound_rates.write().remove(client_id);
        debug!("Disconnected client (keeping subscriptions): {}", client_id);
    }

    pub async fn unregister_client(&self, client_id: &str) {
        let mut clients = self.clients.write().await;
        clients.remove(client_id);
        self.outbound_rates.write().remove(client_id);

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

    pub async fn cleanup_stale_subscriptions(&self) {
        let clients = self.clients.read().await;

        let subscribed_ids: HashSet<String> = {
            let exact = self.exact_subscriptions.read().await;
            let wildcard = self.wildcard_subscriptions.read().await;
            exact
                .values()
                .flatten()
                .chain(wildcard.values().flatten())
                .map(|sub| sub.client_id.clone())
                .collect()
        };

        let mut stale: Vec<String> = Vec::new();
        let storage = self.storage.as_ref();

        for client_id in &subscribed_ids {
            if clients.contains_key(client_id) {
                continue;
            }
            let has_session = if let Some(storage) = storage {
                matches!(storage.get_session(client_id).await, Ok(Some(s)) if !s.is_expired())
            } else {
                false
            };
            if !has_session {
                stale.push(client_id.clone());
            }
        }

        drop(clients);

        if stale.is_empty() {
            return;
        }

        {
            let mut exact = self.exact_subscriptions.write().await;
            for subs in exact.values_mut() {
                subs.retain(|sub| !stale.contains(&sub.client_id));
            }
            exact.retain(|_, subs| !subs.is_empty());
        }

        {
            let mut wildcard = self.wildcard_subscriptions.write().await;
            for subs in wildcard.values_mut() {
                subs.retain(|sub| !stale.contains(&sub.client_id));
            }
            wildcard.retain(|_, subs| !subs.is_empty());
        }

        {
            let mut change_only = self.change_only_states.write().await;
            for client_id in &stale {
                change_only.remove(client_id);
            }
        }

        if let Some(storage) = storage {
            for client_id in &stale {
                if let Err(e) = storage.remove_queued_messages(client_id).await {
                    debug!("failed to remove queued messages for stale client {client_id}: {e}");
                }
            }
        }

        info!(
            "Cleaned up stale subscriptions for {} disconnected client(s)",
            stale.len()
        );
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
        change_only: bool,
        flow_id: Option<u64>,
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
            change_only,
            flow_id,
        };

        let is_new = if Self::has_wildcards(actual_filter) {
            let mut wildcard = self.wildcard_subscriptions.write().await;
            let subs = wildcard.entry(actual_filter.to_string()).or_default();
            let existing_pos = subs
                .iter()
                .position(|s| s.client_id == client_id && s.flow_id == flow_id);
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
            let existing_pos = subs
                .iter()
                .position(|s| s.client_id == client_id && s.flow_id == flow_id);
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

    pub async fn unsubscribe(
        &self,
        client_id: &str,
        topic_filter: &str,
        flow_id: Option<u64>,
    ) -> bool {
        let (actual_filter, _) = parse_shared_subscription(topic_filter);

        let subscriptions = if Self::has_wildcards(actual_filter) {
            &self.wildcard_subscriptions
        } else {
            &self.exact_subscriptions
        };

        let mut subs_map = subscriptions.write().await;

        if let Some(subs) = subs_map.get_mut(actual_filter) {
            let initial_len = subs.len();
            subs.retain(|sub| !(sub.client_id == client_id && sub.flow_id == flow_id));

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

    pub async fn unsubscribe_by_flow(&self, client_id: &str, flow_id: u64) -> Vec<String> {
        let mut removed_filters = Vec::new();

        {
            let mut exact = self.exact_subscriptions.write().await;
            for (filter, subs) in exact.iter_mut() {
                let before = subs.len();
                subs.retain(|sub| !(sub.client_id == client_id && sub.flow_id == Some(flow_id)));
                if subs.len() < before {
                    removed_filters.push(filter.clone());
                }
            }
            exact.retain(|_, subs| !subs.is_empty());
        }

        {
            let mut wildcard = self.wildcard_subscriptions.write().await;
            for (filter, subs) in wildcard.iter_mut() {
                let before = subs.len();
                subs.retain(|sub| !(sub.client_id == client_id && sub.flow_id == Some(flow_id)));
                if subs.len() < before {
                    removed_filters.push(filter.clone());
                }
            }
            wildcard.retain(|_, subs| !subs.is_empty());
        }

        if !removed_filters.is_empty() {
            debug!(
                "Removed {} flow-bound subscriptions for client {} flow {}",
                removed_filters.len(),
                client_id,
                flow_id
            );
        }
        removed_filters
    }

    /// Routes a publish message to all matching subscribers and forwards to bridges.
    pub async fn route_message(&self, publish: &PublishPacket, publishing_client_id: Option<&str>) {
        #[cfg(feature = "opentelemetry")]
        {
            use tracing::Instrument;
            let span = tracing::info_span!(
                "mqtt.route",
                mqtt.topic = %publish.topic_name,
                mqtt.qos = publish.qos as u8,
                mqtt.retain = publish.retain,
            );
            self.route_message_internal(publish, publishing_client_id, true)
                .instrument(span)
                .await;
        }
        #[cfg(not(feature = "opentelemetry"))]
        self.route_message_internal(publish, publishing_client_id, true)
            .await;
    }

    /// Routes a publish message to local subscribers only, without forwarding to bridges.
    ///
    /// Used by bridge connections to prevent message loops when receiving messages
    /// from remote brokers.
    pub async fn route_message_local_only(
        &self,
        publish: &PublishPacket,
        publishing_client_id: Option<&str>,
    ) {
        #[cfg(feature = "opentelemetry")]
        {
            use tracing::Instrument;
            let span = tracing::info_span!(
                "mqtt.route",
                mqtt.topic = %publish.topic_name,
                mqtt.qos = publish.qos as u8,
                mqtt.retain = publish.retain,
                mqtt.bridge_forward = false,
            );
            self.route_message_internal(publish, publishing_client_id, false)
                .instrument(span)
                .await;
        }
        #[cfg(not(feature = "opentelemetry"))]
        self.route_message_internal(publish, publishing_client_id, false)
            .await;
    }

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
            self.handle_retain_storage(publish).await;
        }

        let exact = self.exact_subscriptions.read().await;
        let wildcard = self.wildcard_subscriptions.read().await;
        let clients = self.clients.read().await;

        let (share_groups, regular_subs) =
            Self::collect_matching_subscriptions(&exact, &wildcard, &publish.topic_name);

        self.deliver_share_groups(&share_groups, publish, &clients, publishing_client_id)
            .await;

        for sub in &regular_subs {
            #[cfg(feature = "opentelemetry")]
            {
                use tracing::Instrument;
                let span = tracing::info_span!(
                    "mqtt.deliver",
                    mqtt.subscriber = %sub.client_id,
                    mqtt.topic = %publish.topic_name,
                );
                self.deliver_to_subscriber(
                    sub,
                    publish,
                    &clients,
                    self.storage.as_ref(),
                    publishing_client_id,
                )
                .instrument(span)
                .await;
            }
            #[cfg(not(feature = "opentelemetry"))]
            self.deliver_to_subscriber(
                sub,
                publish,
                &clients,
                self.storage.as_ref(),
                publishing_client_id,
            )
            .await;
        }

        drop(exact);
        drop(wildcard);
        drop(clients);

        if forward_to_bridges {
            #[cfg(feature = "opentelemetry")]
            {
                use tracing::Instrument;
                let span = tracing::info_span!(
                    "mqtt.bridge.forward",
                    mqtt.topic = %publish.topic_name,
                );
                self.forward_to_bridges(publish).instrument(span).await;
            }
            #[cfg(not(feature = "opentelemetry"))]
            self.forward_to_bridges(publish).await;
        }
    }

    async fn handle_retain_storage(&self, publish: &PublishPacket) {
        let (should_remove, retained_msg) = if publish.payload.is_empty() {
            self.retained_messages
                .write()
                .await
                .remove(&publish.topic_name);
            debug!("Deleted retained message for topic: {}", publish.topic_name);
            (true, None)
        } else {
            let retained_msg = RetainedMessage::new(publish.clone());
            self.retained_messages
                .write()
                .await
                .insert(publish.topic_name.clone(), retained_msg.clone());
            debug!("Stored retained message for topic: {}", publish.topic_name);
            (false, Some(retained_msg))
        };

        if let Some(ref storage) = self.storage {
            if should_remove {
                if let Err(e) = storage.remove_retained_message(&publish.topic_name).await {
                    error!("Failed to remove retained message from storage: {e}");
                }
            } else if let Some(ref msg) = retained_msg {
                if let Err(e) = storage
                    .store_retained_message(&publish.topic_name, msg.clone())
                    .await
                {
                    error!("Failed to store retained message to storage: {e}");
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

    fn collect_matching_subscriptions<'a>(
        exact: &'a HashMap<String, Vec<Subscription>>,
        wildcard: &'a HashMap<String, Vec<Subscription>>,
        topic_name: &str,
    ) -> (
        HashMap<String, Vec<&'a Subscription>>,
        Vec<&'a Subscription>,
    ) {
        let mut share_groups: HashMap<String, Vec<&'a Subscription>> = HashMap::new();
        let mut regular_subs: Vec<&'a Subscription> = Vec::new();

        if let Some(subs) = exact.get(topic_name) {
            for sub in subs {
                if let Some(ref group) = sub.share_group {
                    share_groups.entry(group.clone()).or_default().push(sub);
                } else {
                    regular_subs.push(sub);
                }
            }
        }

        for (topic_filter, subs) in wildcard {
            if topic_matches_filter(topic_name, topic_filter) {
                for sub in subs {
                    if let Some(ref group) = sub.share_group {
                        share_groups.entry(group.clone()).or_default().push(sub);
                    } else {
                        regular_subs.push(sub);
                    }
                }
            }
        }

        (share_groups, regular_subs)
    }

    async fn deliver_share_groups(
        &self,
        share_groups: &HashMap<String, Vec<&Subscription>>,
        publish: &PublishPacket,
        clients: &HashMap<String, ClientInfo>,
        publishing_client_id: Option<&str>,
    ) {
        for (group_name, group_subs) in share_groups {
            let online_subs: Vec<&&Subscription> = group_subs
                .iter()
                .filter(|sub| clients.contains_key(&sub.client_id))
                .collect();

            if !online_subs.is_empty() {
                let counter = self
                    .share_group_counters
                    .read()
                    .await
                    .get(group_name)
                    .cloned();
                if let Some(counter) = counter {
                    let index = counter.fetch_add(1, Ordering::Relaxed) % online_subs.len();
                    let chosen_sub = online_subs[index];

                    #[cfg(feature = "opentelemetry")]
                    {
                        use tracing::Instrument;
                        let span = tracing::info_span!(
                            "mqtt.deliver",
                            mqtt.subscriber = %chosen_sub.client_id,
                            mqtt.topic = %publish.topic_name,
                            mqtt.shared_group = %group_name,
                        );
                        self.deliver_to_subscriber(
                            chosen_sub,
                            publish,
                            clients,
                            self.storage.as_ref(),
                            publishing_client_id,
                        )
                        .instrument(span)
                        .await;
                    }
                    #[cfg(not(feature = "opentelemetry"))]
                    self.deliver_to_subscriber(
                        chosen_sub,
                        publish,
                        clients,
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
    }

    async fn forward_to_bridges(&self, publish: &PublishPacket) {
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

    fn effective_qos(publish_qos: QoS, sub_qos: QoS) -> QoS {
        match (publish_qos, sub_qos) {
            (QoS::AtMostOnce, _) | (_, QoS::AtMostOnce) => QoS::AtMostOnce,
            (QoS::AtLeastOnce | QoS::ExactlyOnce, QoS::AtLeastOnce)
            | (QoS::AtLeastOnce, QoS::ExactlyOnce) => QoS::AtLeastOnce,
            (QoS::ExactlyOnce, QoS::ExactlyOnce) => QoS::ExactlyOnce,
        }
    }

    fn prepare_message(publish: &PublishPacket, sub: &Subscription, qos: QoS) -> PublishPacket {
        let mut message = publish.clone();
        message.qos = qos;
        message.dup = false;
        message.protocol_version = sub.protocol_version.as_u8();
        if !sub.retain_as_published {
            message.retain = false;
        }
        if let Some(id) = sub.subscription_id {
            message.properties.set_subscription_identifier(id);
        }
        message
    }

    async fn queue_message(
        storage: &Arc<DynamicStorage>,
        message: PublishPacket,
        client_id: &str,
        qos: QoS,
    ) {
        let queued_msg = QueuedMessage::new(message, client_id.to_string(), qos, None);
        if let Err(e) = storage.queue_message(queued_msg).await {
            error!("Failed to queue message for offline client {client_id}: {e}");
        } else {
            debug!("Queued message for client {client_id}");
        }
    }

    async fn deliver_to_subscriber(
        &self,
        sub: &Subscription,
        publish: &PublishPacket,
        clients: &HashMap<String, ClientInfo>,
        storage: Option<&Arc<DynamicStorage>>,
        publishing_client_id: Option<&str>,
    ) {
        if sub.no_local && publishing_client_id == Some(&sub.client_id) {
            trace!(
                "Skipping delivery to {} due to No Local flag",
                sub.client_id
            );
            return;
        }

        {
            let suppression_key = self.echo_suppression_key.read().await;
            if let Some(ref key) = *suppression_key {
                if let Some(origin) = publish.properties.get_user_property_value(key) {
                    if origin == sub.client_id {
                        trace!("Skipping echo delivery to {}", sub.client_id);
                        return;
                    }
                }
            }
        }

        if sub.change_only {
            let change_only_states = self.change_only_states.read().await;
            if let Some(state) = change_only_states.get(&sub.client_id) {
                if !state.should_deliver(&publish.topic_name, &publish.payload) {
                    trace!(
                        "Skipping change-only delivery to {} - payload unchanged for topic {}",
                        sub.client_id,
                        publish.topic_name
                    );
                    return;
                }
            }
            drop(change_only_states);
        }

        let max_rate = self.max_outbound_rate.load(Ordering::Relaxed);
        if max_rate > 0 {
            let rate_exceeded = {
                let rates = self.outbound_rates.read();
                rates
                    .get(&sub.client_id)
                    .is_some_and(|state| !state.check_and_increment(max_rate))
            };
            if rate_exceeded {
                let qos = Self::effective_qos(publish.qos, sub.qos);
                if qos != QoS::AtMostOnce {
                    if let Some(storage) = storage {
                        let message = Self::prepare_message(publish, sub, qos);
                        Self::queue_message(storage, message, &sub.client_id, qos).await;
                    }
                }
                return;
            }
        }

        if let Some(client_info) = clients.get(&sub.client_id) {
            let qos = Self::effective_qos(publish.qos, sub.qos);
            let message = Self::prepare_message(publish, sub, qos);

            let routable = RoutableMessage {
                publish: message,
                target_flow: sub.flow_id,
            };
            if let Err(e) = client_info.sender.try_send(routable) {
                warn!(
                    client_id = %sub.client_id,
                    topic = %publish.topic_name,
                    "Channel send failed - message may be dropped"
                );
                if let Some(storage) = storage {
                    if qos != QoS::AtMostOnce {
                        Self::queue_message(storage, e.into_inner().publish, &sub.client_id, qos)
                            .await;
                    }
                }
            } else if sub.change_only {
                let mut change_only_states = self.change_only_states.write().await;
                change_only_states
                    .entry(sub.client_id.clone())
                    .or_default()
                    .update_hash(&publish.topic_name, &publish.payload);
            }
        } else if let Some(storage) = storage {
            if sub.qos != QoS::AtMostOnce {
                let message = Self::prepare_message(publish, sub, sub.qos);
                Self::queue_message(storage, message, &sub.client_id, sub.qos).await;
            }
        } else {
            debug!(
                "No storage configured, cannot queue message for offline client {}",
                sub.client_id
            );
        }
    }

    pub async fn get_retained_messages(&self, topic_filter: &str) -> Vec<PublishPacket> {
        let retained = self.retained_messages.read().await;
        retained
            .iter()
            .filter(|(topic, msg)| topic_matches_filter(topic, topic_filter) && !msg.is_expired())
            .map(|(_, msg)| msg.to_publish_packet())
            .collect()
    }

    pub async fn client_count(&self) -> usize {
        self.clients.read().await.len()
    }

    pub async fn topic_count(&self) -> usize {
        let exact = self.exact_subscriptions.read().await;
        let wildcard = self.wildcard_subscriptions.read().await;
        exact.len() + wildcard.len()
    }

    pub async fn retained_count(&self) -> usize {
        self.retained_messages.read().await.len()
    }

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

    pub async fn has_retained_message(&self, topic: &str) -> bool {
        let retained = self.retained_messages.read().await;
        retained.contains_key(topic)
    }

    pub async fn load_change_only_state(&self, client_id: &str, state: ChangeOnlyState) {
        self.change_only_states
            .write()
            .await
            .insert(client_id.to_string(), state);
    }

    pub async fn get_change_only_state(&self, client_id: &str) -> Option<ChangeOnlyState> {
        self.change_only_states.read().await.get(client_id).cloned()
    }

    pub async fn remove_change_only_state(&self, client_id: &str) {
        self.change_only_states.write().await.remove(client_id);
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
                false,
                None,
            )
            .await
            .unwrap();

        assert_eq!(router.topic_count().await, 1);

        let removed = router.unsubscribe("client1", "test/+", None).await;
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
                false,
                None,
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
                false,
                None,
            )
            .await
            .unwrap();

        // Publish message
        let publish = PublishPacket::new("test/data", &b"hello"[..], QoS::ExactlyOnce);

        router.route_message(&publish, None).await;

        // Client 1 should receive with QoS 1 (downgraded)
        let rm1 = rx1.try_recv().unwrap();
        assert_eq!(rm1.publish.topic_name, "test/data");
        assert_eq!(rm1.publish.qos, QoS::AtLeastOnce);

        let rm2 = rx2.try_recv().unwrap();
        assert_eq!(rm2.publish.topic_name, "test/data");
        assert_eq!(rm2.publish.qos, QoS::ExactlyOnce);
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
                false,
                None,
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
                false,
                None,
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
                false,
                None,
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
                false,
                None,
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
                false,
                None,
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
                false,
                None,
            )
            .await
            .unwrap();

        // Publish message
        let publish = PublishPacket::new("test/data", &b"hello"[..], QoS::AtMostOnce);
        router.route_message(&publish, None).await;

        // Regular subscriber should receive the message
        let regular_rm = rx3.try_recv().unwrap();
        assert_eq!(&regular_rm.publish.payload[..], b"hello");

        // Only one of the shared subscribers should receive it
        let shared1_received = rx1.try_recv().is_ok();
        let shared2_received = rx2.try_recv().is_ok();

        assert!(shared1_received ^ shared2_received); // XOR - exactly one should be true
    }

    #[tokio::test]
    async fn test_route_message_local_only_delivers_to_subscribers() {
        let router = MessageRouter::new();
        let (tx1, rx1) = flume::bounded(100);
        let (tx2, rx2) = flume::bounded(100);

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
                false,
                None,
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
                false,
                None,
            )
            .await
            .unwrap();

        let publish = PublishPacket::new("test/data", &b"local-only"[..], QoS::ExactlyOnce);

        router.route_message_local_only(&publish, None).await;

        let rm1 = rx1.try_recv().unwrap();
        assert_eq!(rm1.publish.topic_name, "test/data");
        assert_eq!(&rm1.publish.payload[..], b"local-only");
        assert_eq!(rm1.publish.qos, QoS::AtLeastOnce);

        let rm2 = rx2.try_recv().unwrap();
        assert_eq!(rm2.publish.topic_name, "test/data");
        assert_eq!(&rm2.publish.payload[..], b"local-only");
        assert_eq!(rm2.publish.qos, QoS::ExactlyOnce);
    }

    #[tokio::test]
    async fn test_echo_suppression_skips_matching_client() {
        let router = MessageRouter::new().with_echo_suppression_key("x-origin".to_string());
        let (tx1, rx1) = flume::bounded(100);
        let (tx2, rx2) = flume::bounded(100);

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
                "test/echo".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                None,
            )
            .await
            .unwrap();
        router
            .subscribe(
                "client2".to_string(),
                "test/echo".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                None,
            )
            .await
            .unwrap();

        let mut publish = PublishPacket::new("test/echo", &b"hello"[..], QoS::AtMostOnce);
        publish
            .properties
            .add_user_property("x-origin".to_string(), "client1".to_string());

        router.route_message(&publish, None).await;

        assert!(rx1.try_recv().is_err());

        let rm2 = rx2.try_recv().unwrap();
        assert_eq!(rm2.publish.topic_name, "test/echo");
    }

    #[tokio::test]
    async fn test_echo_suppression_disabled_delivers_all() {
        let router = MessageRouter::new();
        let (tx1, rx1) = flume::bounded(100);
        let (tx2, rx2) = flume::bounded(100);

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
                "test/echo".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                None,
            )
            .await
            .unwrap();
        router
            .subscribe(
                "client2".to_string(),
                "test/echo".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                None,
            )
            .await
            .unwrap();

        let mut publish = PublishPacket::new("test/echo", &b"hello"[..], QoS::AtMostOnce);
        publish
            .properties
            .add_user_property("x-origin".to_string(), "client1".to_string());

        router.route_message(&publish, None).await;

        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_ok());
    }

    #[tokio::test]
    async fn test_route_message_local_only_stores_retained() {
        let router = MessageRouter::new();

        let mut publish = PublishPacket::new("test/status", &b"online"[..], QoS::AtMostOnce);
        publish.retain = true;

        router.route_message_local_only(&publish, None).await;

        assert_eq!(router.retained_count().await, 1);

        let retained = router.get_retained_messages("test/status").await;
        assert_eq!(retained.len(), 1);
        assert_eq!(&retained[0].payload[..], b"online");
    }

    #[tokio::test]
    async fn test_outbound_rate_qos0_dropped_when_exceeded() {
        let router = MessageRouter::new().with_max_outbound_rate(5);
        let (tx, rx) = flume::bounded(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router.register_client("sub1".to_string(), tx, dtx).await;
        router
            .subscribe(
                "sub1".to_string(),
                "test/rate".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                None,
            )
            .await
            .unwrap();

        for i in 0..20u8 {
            let publish =
                PublishPacket::new("test/rate", Bytes::copy_from_slice(&[i]), QoS::AtMostOnce);
            router.route_message(&publish, None).await;
        }

        let mut received = 0;
        while rx.try_recv().is_ok() {
            received += 1;
        }
        assert!(received <= 5, "expected at most 5 messages, got {received}");
        assert!(received >= 1, "expected at least 1 message, got {received}");
    }

    #[tokio::test]
    async fn test_outbound_rate_qos1_queued_when_exceeded() {
        let storage = Arc::new(DynamicStorage::Memory(
            crate::broker::storage::MemoryBackend::new(),
        ));
        let router = MessageRouter::with_storage(Arc::clone(&storage)).with_max_outbound_rate(3);
        let (tx, rx) = flume::bounded(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router.register_client("sub1".to_string(), tx, dtx).await;
        router
            .subscribe(
                "sub1".to_string(),
                "test/rate".to_string(),
                QoS::AtLeastOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                None,
            )
            .await
            .unwrap();

        for i in 0..10u8 {
            let publish =
                PublishPacket::new("test/rate", Bytes::copy_from_slice(&[i]), QoS::AtLeastOnce);
            router.route_message(&publish, None).await;
        }

        let mut delivered = 0;
        while rx.try_recv().is_ok() {
            delivered += 1;
        }
        assert!(
            delivered <= 3,
            "expected at most 3 delivered, got {delivered}"
        );

        let queued = storage.get_queued_messages("sub1").await.unwrap();
        assert!(
            !queued.is_empty(),
            "expected queued messages for rate-limited QoS 1"
        );
        assert_eq!(
            delivered + queued.len(),
            10,
            "delivered + queued should equal total sent"
        );
    }

    #[tokio::test]
    async fn test_outbound_rate_zero_means_unlimited() {
        let router = MessageRouter::new().with_max_outbound_rate(0);
        let (tx, rx) = flume::bounded(1000);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router.register_client("sub1".to_string(), tx, dtx).await;
        router
            .subscribe(
                "sub1".to_string(),
                "test/rate".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                None,
            )
            .await
            .unwrap();

        for i in 0..100u8 {
            let publish =
                PublishPacket::new("test/rate", Bytes::copy_from_slice(&[i]), QoS::AtMostOnce);
            router.route_message(&publish, None).await;
        }

        let mut received = 0;
        while rx.try_recv().is_ok() {
            received += 1;
        }
        assert_eq!(received, 100);
    }

    #[tokio::test]
    async fn test_outbound_rate_cleanup_on_unregister() {
        let router = MessageRouter::new().with_max_outbound_rate(10);
        let (tx, _rx) = flume::bounded(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router.register_client("sub1".to_string(), tx, dtx).await;
        assert!(router.outbound_rates.read().contains_key("sub1"));

        router.unregister_client("sub1").await;
        assert!(!router.outbound_rates.read().contains_key("sub1"));
    }

    #[tokio::test]
    async fn test_flow_bound_subscription_delivers_with_target_flow() {
        let router = MessageRouter::new();
        let (tx, rx) = flume::bounded(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router.register_client("c1".to_string(), tx, dtx).await;

        router
            .subscribe(
                "c1".to_string(),
                "sensor/#".to_string(),
                QoS::AtLeastOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                Some(42),
            )
            .await
            .unwrap();

        let publish = PublishPacket::new("sensor/temp", &b"25C"[..], QoS::AtMostOnce);
        router.route_message(&publish, Some("pub1")).await;

        let routable = rx.recv_async().await.unwrap();
        assert_eq!(routable.target_flow, Some(42));
        assert_eq!(routable.publish.topic_name, "sensor/temp");
    }

    #[tokio::test]
    async fn test_flow_and_control_subscriptions_coexist() {
        let router = MessageRouter::new();
        let (tx, rx) = flume::bounded(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router.register_client("c1".to_string(), tx, dtx).await;

        router
            .subscribe(
                "c1".to_string(),
                "data/#".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                None,
            )
            .await
            .unwrap();

        router
            .subscribe(
                "c1".to_string(),
                "data/#".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                Some(7),
            )
            .await
            .unwrap();

        let publish = PublishPacket::new("data/x", &b"val"[..], QoS::AtMostOnce);
        router.route_message(&publish, Some("pub1")).await;

        let msg1 = rx.recv_async().await.unwrap();
        let msg2 = rx.recv_async().await.unwrap();
        let flows: Vec<Option<u64>> = vec![msg1.target_flow, msg2.target_flow];
        assert!(flows.contains(&None));
        assert!(flows.contains(&Some(7)));
    }

    #[tokio::test]
    async fn test_unsubscribe_by_flow_removes_only_flow_subs() {
        let router = MessageRouter::new();
        let (tx, rx) = flume::bounded(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router.register_client("c1".to_string(), tx, dtx).await;

        router
            .subscribe(
                "c1".to_string(),
                "topic/a".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                None,
            )
            .await
            .unwrap();

        router
            .subscribe(
                "c1".to_string(),
                "topic/a".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                Some(10),
            )
            .await
            .unwrap();

        router
            .subscribe(
                "c1".to_string(),
                "topic/b".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                Some(10),
            )
            .await
            .unwrap();

        let removed = router.unsubscribe_by_flow("c1", 10).await;
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&"topic/a".to_string()));
        assert!(removed.contains(&"topic/b".to_string()));

        let publish = PublishPacket::new("topic/a", &b"data"[..], QoS::AtMostOnce);
        router.route_message(&publish, Some("pub1")).await;

        let routable = rx.recv_async().await.unwrap();
        assert_eq!(routable.target_flow, None);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_flow_dedup_same_topic_same_flow() {
        let router = MessageRouter::new();
        let (tx, rx) = flume::bounded(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router.register_client("c1".to_string(), tx, dtx).await;

        router
            .subscribe(
                "c1".to_string(),
                "dup/test".to_string(),
                QoS::AtMostOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                Some(5),
            )
            .await
            .unwrap();

        router
            .subscribe(
                "c1".to_string(),
                "dup/test".to_string(),
                QoS::AtLeastOnce,
                None,
                false,
                false,
                0,
                ProtocolVersion::V5,
                false,
                Some(5),
            )
            .await
            .unwrap();

        let publish = PublishPacket::new("dup/test", &b"once"[..], QoS::AtMostOnce);
        router.route_message(&publish, Some("pub1")).await;

        let routable = rx.recv_async().await.unwrap();
        assert_eq!(routable.target_flow, Some(5));
        assert!(rx.try_recv().is_err());
    }
}
