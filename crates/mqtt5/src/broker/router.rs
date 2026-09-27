#[cfg(not(target_arch = "wasm32"))]
use crate::broker::bridge::BridgeManager;
use crate::broker::events::{BrokerEventHandler, RetainedSetEvent};
use crate::broker::session_slot::{SessionSlotGuard, SessionSlots};
use crate::broker::storage::{
    unix_millis_now, ChangeOnlyState, ClientSession, DynamicStorage, QueueHandle, QueueLimits,
    QueueRegistry, QueuedMessage, RetainedMessage, StorageBackend, StoredSubscription,
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
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info, trace, warn};

/// Upper bound on how long one publish may wait for slow subscribers' delivery channels.
pub const ROUTE_BUDGET_MAX: Duration = Duration::from_secs(2);

fn default_route_deadline() -> Option<Instant> {
    cfg!(not(target_arch = "wasm32")).then(|| Instant::now() + ROUTE_BUDGET_MAX)
}

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

/// Parameters of one subscription registration passed to [`MessageRouter::subscribe`].
#[derive(Debug, Clone)]
pub struct SubscriptionRequest {
    pub client_id: String,
    pub topic_filter: String,
    pub qos: QoS,
    pub subscription_id: Option<u32>,
    pub no_local: bool,
    pub retain_as_published: bool,
    pub retain_handling: u8,
    pub protocol_version: ProtocolVersion,
    pub change_only: bool,
    pub flow_id: Option<u64>,
}

impl SubscriptionRequest {
    /// Creates a request with no subscription identifier, all subscription options cleared,
    /// `retain_handling` 0, MQTT v5.0, no change-only delivery and no flow.
    #[must_use]
    pub fn new(client_id: impl Into<String>, topic_filter: impl Into<String>, qos: QoS) -> Self {
        Self {
            client_id: client_id.into(),
            topic_filter: topic_filter.into(),
            qos,
            subscription_id: None,
            no_local: false,
            retain_as_published: false,
            retain_handling: 0,
            protocol_version: ProtocolVersion::default(),
            change_only: false,
            flow_id: None,
        }
    }

    #[must_use]
    pub fn with_subscription_id(mut self, subscription_id: Option<u32>) -> Self {
        self.subscription_id = subscription_id;
        self
    }

    #[must_use]
    pub fn with_no_local(mut self, no_local: bool) -> Self {
        self.no_local = no_local;
        self
    }

    #[must_use]
    pub fn with_retain_as_published(mut self, retain_as_published: bool) -> Self {
        self.retain_as_published = retain_as_published;
        self
    }

    #[must_use]
    pub fn with_retain_handling(mut self, retain_handling: u8) -> Self {
        self.retain_handling = retain_handling;
        self
    }

    #[must_use]
    pub fn with_protocol_version(mut self, protocol_version: ProtocolVersion) -> Self {
        self.protocol_version = protocol_version;
        self
    }

    #[must_use]
    pub fn with_change_only(mut self, change_only: bool) -> Self {
        self.change_only = change_only;
        self
    }

    #[must_use]
    pub fn with_flow_id(mut self, flow_id: Option<u64>) -> Self {
        self.flow_id = flow_id;
        self
    }
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
    wasm_bridge_callback: RwLock<Option<WasmBridgeCallback>>,
    echo_suppression_key: Arc<RwLock<Option<String>>>,
    outbound_rates: parking_lot::RwLock<HashMap<String, OutboundRateState>>,
    max_outbound_rate: AtomicU32,
    fallback_queues: QueueRegistry,
    next_generation: std::sync::atomic::AtomicU64,
    pending_wills: parking_lot::Mutex<HashMap<String, PendingWill>>,
    session_slots: SessionSlots,
}

struct PendingWill {
    generation: u64,
    cancel: oneshot::Sender<()>,
}

const GENERATIONS_PER_MILLISECOND: u64 = 1 << 20;
const RECOVERY_CONCURRENCY: usize = 1024;

fn generation_epoch() -> u64 {
    unix_millis_now().saturating_mul(GENERATIONS_PER_MILLISECOND)
}

/// Information about a connected client
#[derive(Debug)]
pub struct ClientInfo {
    pub generation: u64,
    pub qos1_tx: mpsc::Sender<RoutableMessage>,
    pub qos0_tx: mpsc::Sender<RoutableMessage>,
    pub queue: QueueHandle,
    pub disconnect_tx: oneshot::Sender<TakeoverNotice>,
}

/// Balances one `begin_handoff`: whoever ends up holding the `TakeoverNotice` decrements the
/// count when it completes the hand-off or is dropped, so a displaced handler that panics
/// before handing off (its `disconnect_rx`, and the notice inside it, unwind with the task)
/// can never leave the count stuck above zero and wedge the client id forever.
#[derive(Debug)]
pub struct HandoffGuard {
    queue: QueueHandle,
}

impl HandoffGuard {
    fn new(queue: QueueHandle) -> Self {
        queue.begin_handoff();
        Self { queue }
    }
}

impl Drop for HandoffGuard {
    fn drop(&mut self) {
        self.queue.end_handoff();
    }
}

/// Delivered to the handler a newer connection with the same client id displaces.
///
/// The displaced handler hands its unfinished deliveries back to the session queue (or drops
/// them when `discard` is set), then signals `released` so the new handler may start delivering.
/// The `guard` decrements the queue's hand-off count when the notice is completed or dropped.
#[derive(Debug)]
pub struct TakeoverNotice {
    pub discard: bool,
    pub released: oneshot::Sender<()>,
    pub guard: HandoffGuard,
}

/// What a handler learns when it registers its connection.
#[derive(Debug)]
pub struct Registration {
    pub generation: u64,
    pub released: Option<oneshot::Receiver<()>>,
}

/// Whether the releasing handler still owned the router entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    Owned,
    Displaced,
}

/// Outcome of a generation-fenced subscribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subscribed {
    New,
    Updated,
    Fenced,
}

/// Outcome of a generation-fenced unsubscribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsubscribed {
    Removed,
    Absent,
    Fenced,
}

/// Senders for one client's two delivery lanes.
#[derive(Debug, Clone)]
pub struct DeliveryLanes {
    pub qos1_tx: mpsc::Sender<RoutableMessage>,
    pub qos0_tx: mpsc::Sender<RoutableMessage>,
}

impl DeliveryLanes {
    /// Creates both lanes with the given capacity each.
    #[must_use]
    pub fn channel(capacity: usize) -> (Self, LaneReceivers) {
        let (qos1_tx, qos1_rx) = mpsc::channel(capacity);
        let (qos0_tx, qos0_rx) = mpsc::channel(capacity);
        (
            Self { qos1_tx, qos0_tx },
            LaneReceivers {
                qos1: qos1_rx,
                qos0: qos0_rx,
            },
        )
    }
}

/// Receivers for one client's two delivery lanes, with helpers that read either lane.
#[derive(Debug)]
pub struct LaneReceivers {
    pub qos1: mpsc::Receiver<RoutableMessage>,
    pub qos0: mpsc::Receiver<RoutableMessage>,
}

impl LaneReceivers {
    /// Takes the next message from the gated lane, then the ungated lane, without waiting.
    ///
    /// # Errors
    /// Returns the ungated lane's error when both lanes are empty or closed.
    pub fn try_recv(&mut self) -> std::result::Result<RoutableMessage, mpsc::error::TryRecvError> {
        self.qos1.try_recv().or_else(|_| self.qos0.try_recv())
    }

    /// Waits for the next message on either lane.
    pub async fn recv(&mut self) -> Option<RoutableMessage> {
        tokio::select! {
            message = self.qos1.recv() => message,
            message = self.qos0.recv() => message,
        }
    }
}

enum DeliveryPlan {
    Online {
        client_id: String,
        message: PublishPacket,
        target_flow: Option<u64>,
        lanes: DeliveryLanes,
        queue: QueueHandle,
        epoch: u64,
    },
    Behind {
        client_id: String,
        message: PublishPacket,
        target_flow: Option<u64>,
        queue: QueueHandle,
        epoch: u64,
    },
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
            wasm_bridge_callback: RwLock::new(None),
            echo_suppression_key: Arc::new(RwLock::new(None)),
            outbound_rates: parking_lot::RwLock::new(HashMap::new()),
            max_outbound_rate: AtomicU32::new(0),
            fallback_queues: QueueRegistry::new(QueueLimits::default(), None),
            next_generation: std::sync::atomic::AtomicU64::new(generation_epoch()),
            pending_wills: parking_lot::Mutex::new(HashMap::new()),
            session_slots: SessionSlots::default(),
        }
    }

    /// The client's queue: from storage when persistence is on, else an in-memory one.
    #[must_use]
    pub fn queue_handle(&self, client_id: &str) -> QueueHandle {
        self.storage.as_ref().map_or_else(
            || self.fallback_queues.handle(client_id),
            |storage| storage.queue_handle(client_id),
        )
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
            wasm_bridge_callback: RwLock::new(None),
            echo_suppression_key: Arc::new(RwLock::new(None)),
            outbound_rates: parking_lot::RwLock::new(HashMap::new()),
            max_outbound_rate: AtomicU32::new(0),
            fallback_queues: QueueRegistry::new(QueueLimits::default(), None),
            next_generation: std::sync::atomic::AtomicU64::new(generation_epoch()),
            pending_wills: parking_lot::Mutex::new(HashMap::new()),
            session_slots: SessionSlots::default(),
        }
    }

    /// Sets the per-client limits for the in-memory fallback queues used when persistence is
    /// off, so a persistence-less broker honours the configured caps instead of the defaults.
    #[must_use]
    pub fn with_fallback_queue_limits(mut self, limits: QueueLimits) -> Self {
        self.fallback_queues = QueueRegistry::new(limits, None);
        self
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

    /// Registers a connection that resumes (or starts) a session without a clean start.
    pub async fn register_client(
        &self,
        client_id: String,
        lanes: DeliveryLanes,
        queue: QueueHandle,
        disconnect_tx: oneshot::Sender<TakeoverNotice>,
    ) -> Registration {
        self.register_session(client_id, lanes, queue, disconnect_tx, false)
            .await
    }

    /// Registers a connection, displacing any live handler for the same client id.
    ///
    /// While a displaced handler is still handing its deliveries back, the queue's hand-off
    /// flag keeps routers queueing behind it; the returned `released` receiver fires when the
    /// old handler is done. A clean start also drops every subscription the client id still
    /// holds; its handler discards the whole queue when it binds.
    pub async fn register_session(
        &self,
        client_id: String,
        lanes: DeliveryLanes,
        queue: QueueHandle,
        disconnect_tx: oneshot::Sender<TakeoverNotice>,
        clean_start: bool,
    ) -> Registration {
        let generation = self.allocate_generation();
        self.register_session_as(
            generation,
            client_id,
            lanes,
            queue,
            disconnect_tx,
            clean_start,
        )
        .await
    }

    #[must_use]
    pub fn allocate_generation(&self) -> u64 {
        self.next_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1
    }

    pub async fn register_session_as(
        &self,
        generation: u64,
        client_id: String,
        lanes: DeliveryLanes,
        queue: QueueHandle,
        disconnect_tx: oneshot::Sender<TakeoverNotice>,
        clean_start: bool,
    ) -> Registration {
        let subscription_maps = if clean_start {
            let mut exact = self.exact_subscriptions.write().await;
            let mut wildcard = self.wildcard_subscriptions.write().await;
            Self::strip_client(&mut exact, &client_id);
            Self::strip_client(&mut wildcard, &client_id);
            Some((exact, wildcard))
        } else {
            None
        };

        let mut clients = self.clients.write().await;
        self.cancel_pending_will(&client_id);
        let released = match clients.remove(&client_id) {
            Some(old_client) => {
                info!("Client ID takeover: {}", client_id);
                let (released_tx, released_rx) = oneshot::channel();
                let notice = TakeoverNotice {
                    discard: clean_start,
                    released: released_tx,
                    guard: HandoffGuard::new(Arc::clone(&queue)),
                };
                if old_client.disconnect_tx.send(notice).is_ok() {
                    Some(released_rx)
                } else {
                    None
                }
            }
            None => None,
        };

        clients.insert(
            client_id.clone(),
            ClientInfo {
                generation,
                qos1_tx: lanes.qos1_tx,
                qos0_tx: lanes.qos0_tx,
                queue,
                disconnect_tx,
            },
        );
        drop(clients);
        drop(subscription_maps);
        if clean_start {
            self.change_only_states.write().await.remove(&client_id);
        }
        self.outbound_rates
            .write()
            .insert(client_id.clone(), OutboundRateState::new());
        info!("Registered client: {}", client_id);
        Registration {
            generation,
            released,
        }
    }

    pub async fn release_client(
        &self,
        client_id: &str,
        generation: u64,
        preserve_session: bool,
    ) -> Release {
        let mut exact = self.exact_subscriptions.write().await;
        let mut wildcard = self.wildcard_subscriptions.write().await;
        let mut clients = self.clients.write().await;
        if clients
            .get(client_id)
            .is_none_or(|info| info.generation != generation)
        {
            return Release::Displaced;
        }
        clients.remove(client_id);
        if !preserve_session {
            Self::strip_client(&mut exact, client_id);
            Self::strip_client(&mut wildcard, client_id);
        }
        drop(clients);
        drop(wildcard);
        drop(exact);
        self.outbound_rates.write().remove(client_id);
        if preserve_session {
            debug!("Disconnected client (keeping subscriptions): {}", client_id);
        } else {
            debug!("Unregistered client: {}", client_id);
        }
        Release::Owned
    }

    pub async fn lock_session(&self, client_id: &str) -> SessionSlotGuard {
        self.session_slots.lock(client_id).await
    }

    #[must_use]
    pub fn session_slot_count(&self) -> usize {
        self.session_slots.len()
    }

    pub async fn is_current_owner(&self, client_id: &str, generation: u64) -> bool {
        self.clients
            .read()
            .await
            .get(client_id)
            .is_some_and(|info| info.generation == generation)
    }

    pub fn cancel_pending_will(&self, client_id: &str) {
        let Some(pending) = self.pending_wills.lock().remove(client_id) else {
            return;
        };
        debug!(
            client_id,
            armed_by = pending.generation,
            "New connection cancelled pending delayed will"
        );
        if pending.cancel.send(()).is_err() {
            debug!(client_id, "Delayed will timer already gone");
        }
    }

    pub async fn arm_will(
        &self,
        client_id: &str,
        generation: u64,
    ) -> Option<oneshot::Receiver<()>> {
        let clients = self.clients.read().await;
        if clients
            .get(client_id)
            .is_none_or(|info| info.generation != generation)
        {
            return None;
        }
        let (cancel, cancelled) = oneshot::channel();
        self.pending_wills
            .lock()
            .insert(client_id.to_string(), PendingWill { generation, cancel });
        drop(clients);
        Some(cancelled)
    }

    pub async fn claim_will(&self, client_id: &str, generation: u64) -> bool {
        let claimed = {
            let mut pending = self.pending_wills.lock();
            let armed_here = pending
                .get(client_id)
                .is_some_and(|will| will.generation == generation);
            armed_here && pending.remove(client_id).is_some()
        };
        if claimed {
            self.clear_stored_will(client_id, generation).await;
        }
        claimed
    }

    pub async fn clear_stored_will(&self, client_id: &str, generation: u64) {
        let Some(storage) = &self.storage else {
            return;
        };
        let cleared = storage
            .update_session(client_id, generation, |session| {
                session.will_message = None;
                session.will_delay_interval = None;
            })
            .await;
        match cleared {
            Ok(true) => debug!(client_id, "Removed will from stored session"),
            Ok(false) => debug!(
                client_id,
                "Stored session belongs to a newer connection; left its will alone"
            ),
            Err(e) => warn!("Failed to clear stored will for {client_id}: {e}"),
        }
    }

    #[must_use]
    pub fn stored_subscription_request(
        client_id: &str,
        topic_filter: &str,
        stored: &StoredSubscription,
    ) -> SubscriptionRequest {
        SubscriptionRequest::new(client_id.to_string(), topic_filter.to_string(), stored.qos)
            .with_subscription_id(stored.subscription_id)
            .with_no_local(stored.no_local)
            .with_retain_as_published(stored.retain_as_published)
            .with_retain_handling(stored.retain_handling)
            .with_protocol_version(
                ProtocolVersion::try_from(stored.protocol_version).unwrap_or_default(),
            )
            .with_change_only(stored.change_only)
            .with_flow_id(stored.flow_id)
    }

    /// # Errors
    /// Returns an error if a stored subscription carries an invalid `retain_handling`.
    pub async fn set_client_subscriptions(
        &self,
        slot: &SessionSlotGuard,
        session: Option<&ClientSession>,
    ) -> Result<()> {
        let client_id = slot.client_id();
        let requests: Vec<SubscriptionRequest> = session
            .map(|session| {
                session
                    .subscriptions
                    .iter()
                    .map(|(filter, stored)| {
                        Self::stored_subscription_request(client_id, filter, stored)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let keep: HashSet<(String, Option<u64>)> = requests
            .iter()
            .map(|request| {
                (
                    parse_shared_subscription(&request.topic_filter)
                        .0
                        .to_string(),
                    request.flow_id,
                )
            })
            .collect();
        {
            let mut exact = self.exact_subscriptions.write().await;
            let mut wildcard = self.wildcard_subscriptions.write().await;
            for map in [&mut *exact, &mut *wildcard] {
                for (filter, subs) in map.iter_mut() {
                    subs.retain(|sub| {
                        sub.client_id != client_id || keep.contains(&(filter.clone(), sub.flow_id))
                    });
                }
                map.retain(|_, subs| !subs.is_empty());
            }
        }
        for request in requests {
            self.subscribe_as(None, request).await?;
        }
        Ok(())
    }

    pub async fn sweep_sessions(&self) {
        let mut candidates: HashSet<String> = {
            let exact = self.exact_subscriptions.read().await;
            let wildcard = self.wildcard_subscriptions.read().await;
            exact
                .values()
                .flatten()
                .chain(wildcard.values().flatten())
                .map(|sub| sub.client_id.clone())
                .collect()
        };
        if let Some(storage) = &self.storage {
            match storage.session_client_ids().await {
                Ok(ids) => candidates.extend(ids),
                Err(e) => warn!("Failed to list stored sessions for the expiry sweep: {e}"),
            }
        }
        let connected: HashSet<String> = self.clients.read().await.keys().cloned().collect();
        let mut swept = 0usize;
        for client_id in candidates.difference(&connected) {
            let slot = self.lock_session(client_id).await;
            if self.sweep_session(&slot).await {
                swept += 1;
            }
        }
        self.session_slots.prune();
        for queue in self.fallback_queues.handles() {
            queue.purge_expired();
        }
        self.fallback_queues.evict_idle();
        if swept > 0 {
            info!("Swept {swept} expired or ownerless session(s)");
        }
    }

    async fn sweep_session(&self, slot: &SessionSlotGuard) -> bool {
        let client_id = slot.client_id();
        if self.clients.read().await.contains_key(client_id) {
            return false;
        }
        if let Some(storage) = &self.storage {
            match storage.remove_expired_session(client_id).await {
                Ok(removed) if removed => debug!(client_id, "Removed expired session"),
                Ok(_) => {}
                Err(e) => {
                    warn!("Failed to remove expired session for {client_id}: {e}");
                    return false;
                }
            }
            match storage.get_session(client_id).await {
                Ok(Some(session)) => {
                    if session.connected {
                        self.end_stranded_session(storage, &session).await;
                    }
                    return false;
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("Failed to read session for {client_id} during the sweep: {e}");
                    return false;
                }
            }
        }
        let had_subscriptions = self.subscription_count_for_client(client_id).await > 0;
        self.remove_client_subscriptions(client_id).await;
        self.change_only_states.write().await.remove(client_id);
        self.queue_handle(client_id).clear(None);
        had_subscriptions
    }

    async fn end_stranded_session(&self, storage: &DynamicStorage, session: &ClientSession) {
        let client_id = session.client_id.as_str();
        let disconnected_at = unix_millis_now();
        match storage
            .update_session(client_id, session.connection_token, |stored| {
                stored.mark_disconnected(disconnected_at);
            })
            .await
        {
            Ok(true) => info!(
                client_id,
                "Ended a stored session left connected without an owner"
            ),
            Ok(false) => {}
            Err(e) => warn!("Failed to end the ownerless session of {client_id}: {e}"),
        }
    }

    /// # Errors
    /// Returns an error if the stored sessions cannot be listed.
    pub async fn recover_sessions(&self, maximum_expiry: u32) -> Result<usize> {
        use futures::StreamExt;
        let Some(storage) = &self.storage else {
            return Ok(0);
        };
        let boot = unix_millis_now();
        let recovered = futures::stream::iter(storage.session_client_ids().await?)
            .map(|client_id| self.recover_session(storage, client_id, boot, maximum_expiry))
            .buffer_unordered(RECOVERY_CONCURRENCY)
            .filter(|recovered| std::future::ready(*recovered))
            .count()
            .await;
        self.session_slots.prune();
        if recovered > 0 {
            info!("Recovered {recovered} persisted session(s)");
        }
        Ok(recovered)
    }

    async fn recover_session(
        &self,
        storage: &DynamicStorage,
        client_id: String,
        boot: u64,
        maximum_expiry: u32,
    ) -> bool {
        let slot = self.lock_session(&client_id).await;
        if let Err(e) = storage.remove_expired_session(&client_id).await {
            warn!("Failed to remove expired session for {client_id}: {e}");
            return false;
        }
        let mut session = match storage.get_session(&client_id).await {
            Ok(Some(session)) => session,
            Ok(None) => return false,
            Err(e) => {
                warn!("Failed to read session for {client_id} during recovery: {e}");
                return false;
            }
        };
        let stored_expiry = session.expiry_interval;
        let capped = Some(ClientSession::granted_expiry(stored_expiry, maximum_expiry));
        let stamp = session.disconnected_at.is_none();
        if stamp {
            session.mark_disconnected(boot);
        }
        session.expiry_interval = capped;
        if session.expiry_interval == Some(0) || session.is_expired() {
            if let Err(e) = storage
                .remove_owned_session(&client_id, session.connection_token)
                .await
            {
                warn!("Failed to remove ended session for {client_id}: {e}");
            }
            return false;
        }
        if stamp || capped != stored_expiry {
            if let Err(e) = storage.store_session(session.clone()).await {
                warn!("Failed to stamp the disconnect time of {client_id}: {e}");
                return false;
            }
        }
        if let Err(e) = self.set_client_subscriptions(&slot, Some(&session)).await {
            warn!("Failed to restore subscriptions for {client_id}: {e}");
            return false;
        }
        self.load_change_only_state(&client_id, session.change_only_state.clone())
            .await;
        true
    }

    pub async fn is_connected(&self, client_id: &str) -> bool {
        self.clients.read().await.contains_key(client_id)
    }

    pub async fn disconnect_client(&self, client_id: &str) {
        let mut clients = self.clients.write().await;
        clients.remove(client_id);
        self.outbound_rates.write().remove(client_id);
        debug!("Disconnected client (keeping subscriptions): {}", client_id);
    }

    pub async fn unregister_client(&self, client_id: &str) {
        {
            let mut clients = self.clients.write().await;
            clients.remove(client_id);
        }
        self.outbound_rates.write().remove(client_id);
        self.remove_client_subscriptions(client_id).await;
        debug!("Unregistered client: {}", client_id);
    }

    async fn remove_client_subscriptions(&self, client_id: &str) {
        {
            let mut exact = self.exact_subscriptions.write().await;
            Self::strip_client(&mut exact, client_id);
        }
        {
            let mut wildcard = self.wildcard_subscriptions.write().await;
            Self::strip_client(&mut wildcard, client_id);
        }
    }

    fn strip_client(map: &mut HashMap<String, Vec<Subscription>>, client_id: &str) {
        for subs in map.values_mut() {
            subs.retain(|sub| sub.client_id != client_id);
        }
        map.retain(|_, subs| !subs.is_empty());
    }

    /// Adds a subscription for a client.
    ///
    /// # Errors
    /// Returns an error if subscription registration fails or `retain_handling` is invalid.
    pub async fn subscribe(&self, request: SubscriptionRequest) -> Result<bool> {
        let outcome = self.subscribe_as(None, request).await?;
        Ok(outcome == Subscribed::New)
    }

    /// Adds a subscription on behalf of the handler holding `generation`; a handler whose
    /// registration has been displaced gets `Fenced` and changes nothing.
    ///
    /// # Errors
    /// Returns an error when `retain_handling` is invalid.
    pub async fn subscribe_as(
        &self,
        generation: Option<u64>,
        request: SubscriptionRequest,
    ) -> Result<Subscribed> {
        let SubscriptionRequest {
            client_id,
            topic_filter,
            qos,
            subscription_id,
            no_local,
            retain_as_published,
            retain_handling,
            protocol_version,
            change_only,
            flow_id,
        } = request;
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

        let subscriptions = if Self::has_wildcards(actual_filter) {
            &self.wildcard_subscriptions
        } else {
            &self.exact_subscriptions
        };
        let mut subs_map = subscriptions.write().await;
        if !self.generation_is_current(generation, &client_id).await {
            return Ok(Subscribed::Fenced);
        }
        let subs = subs_map.entry(actual_filter.to_string()).or_default();
        let existing_pos = subs
            .iter()
            .position(|s| s.client_id == client_id && s.flow_id == flow_id);
        let outcome = if let Some(pos) = existing_pos {
            subs[pos] = subscription;
            debug!(
                "Client {} updated subscription to {}",
                client_id, topic_filter
            );
            Subscribed::Updated
        } else {
            subs.push(subscription);
            debug!("Client {} subscribed to {}", client_id, topic_filter);
            Subscribed::New
        };
        drop(subs_map);

        if let Some(group) = share_group {
            let mut counters = self.share_group_counters.write().await;
            counters
                .entry(group)
                .or_insert_with(|| Arc::new(AtomicUsize::new(0)));
        }

        Ok(outcome)
    }

    async fn generation_is_current(&self, generation: Option<u64>, client_id: &str) -> bool {
        match generation {
            None => true,
            Some(generation) => self
                .clients
                .read()
                .await
                .get(client_id)
                .is_some_and(|info| info.generation == generation),
        }
    }

    pub async fn unsubscribe(
        &self,
        client_id: &str,
        topic_filter: &str,
        flow_id: Option<u64>,
    ) -> bool {
        self.unsubscribe_as(None, client_id, topic_filter, flow_id)
            .await
            == Unsubscribed::Removed
    }

    /// Removes a subscription on behalf of the handler holding `generation`; a displaced
    /// handler gets `Fenced` and changes nothing.
    pub async fn unsubscribe_as(
        &self,
        generation: Option<u64>,
        client_id: &str,
        topic_filter: &str,
        flow_id: Option<u64>,
    ) -> Unsubscribed {
        let (actual_filter, _) = parse_shared_subscription(topic_filter);

        let subscriptions = if Self::has_wildcards(actual_filter) {
            &self.wildcard_subscriptions
        } else {
            &self.exact_subscriptions
        };

        let mut subs_map = subscriptions.write().await;
        if !self.generation_is_current(generation, client_id).await {
            return Unsubscribed::Fenced;
        }

        if let Some(subs) = subs_map.get_mut(actual_filter) {
            let initial_len = subs.len();
            subs.retain(|sub| !(sub.client_id == client_id && sub.flow_id == flow_id));

            let removed = initial_len != subs.len();

            if subs.is_empty() {
                subs_map.remove(actual_filter);
            }
            if removed {
                debug!("Client {} unsubscribed from {}", client_id, topic_filter);
                Unsubscribed::Removed
            } else {
                Unsubscribed::Absent
            }
        } else {
            Unsubscribed::Absent
        }
    }

    /// Removes every subscription the handler holding `generation` bound to `flow_id`;
    /// a displaced handler removes nothing.
    pub async fn unsubscribe_by_flow(
        &self,
        generation: Option<u64>,
        client_id: &str,
        flow_id: u64,
    ) -> Vec<String> {
        let mut removed_filters = Vec::new();

        let mut exact = self.exact_subscriptions.write().await;
        let mut wildcard = self.wildcard_subscriptions.write().await;
        if !self.generation_is_current(generation, client_id).await {
            return removed_filters;
        }
        for map in [&mut *exact, &mut *wildcard] {
            for (filter, subs) in map.iter_mut() {
                let before = subs.len();
                subs.retain(|sub| !(sub.client_id == client_id && sub.flow_id == Some(flow_id)));
                if subs.len() < before {
                    removed_filters.push(filter.clone());
                }
            }
            map.retain(|_, subs| !subs.is_empty());
        }
        drop(wildcard);
        drop(exact);

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
        self.route_message_bounded(publish, publishing_client_id, default_route_deadline())
            .await;
    }

    /// Routes a publish message; `deadline` bounds the total time spent waiting on slow
    /// subscribers' delivery channels for this one publish.
    pub async fn route_message_with_deadline(
        &self,
        publish: &PublishPacket,
        publishing_client_id: Option<&str>,
        deadline: Instant,
    ) {
        self.route_message_bounded(publish, publishing_client_id, Some(deadline))
            .await;
    }

    async fn route_message_bounded(
        &self,
        publish: &PublishPacket,
        publishing_client_id: Option<&str>,
        deadline: Option<Instant>,
    ) {
        #[cfg(feature = "opentelemetry")]
        {
            use tracing::Instrument;
            let span = tracing::info_span!(
                "mqtt.route",
                mqtt.topic = %publish.topic_name,
                mqtt.qos = publish.qos as u8,
                mqtt.retain = publish.retain,
            );
            self.route_message_internal(publish, publishing_client_id, true, deadline)
                .instrument(span)
                .await;
        }
        #[cfg(not(feature = "opentelemetry"))]
        self.route_message_internal(publish, publishing_client_id, true, deadline)
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
        let deadline = default_route_deadline();
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
            self.route_message_internal(publish, publishing_client_id, false, deadline)
                .instrument(span)
                .await;
        }
        #[cfg(not(feature = "opentelemetry"))]
        self.route_message_internal(publish, publishing_client_id, false, deadline)
            .await;
    }

    async fn route_message_internal(
        &self,
        publish: &PublishPacket,
        publishing_client_id: Option<&str>,
        forward_to_bridges: bool,
        deadline: Option<Instant>,
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

        let plans = {
            let exact = self.exact_subscriptions.read().await;
            let wildcard = self.wildcard_subscriptions.read().await;
            let clients = self.clients.read().await;

            let (share_groups, regular_subs) =
                Self::collect_matching_subscriptions(&exact, &wildcard, &publish.topic_name);

            let mut plans = self
                .plan_share_groups(&share_groups, publish, &clients, publishing_client_id)
                .await;
            for sub in &regular_subs {
                if let Some(plan) = self
                    .plan_delivery(sub, publish, &clients, publishing_client_id)
                    .await
                {
                    plans.push(plan);
                }
            }
            plans
        };

        for plan in plans {
            Self::execute_plan(plan, publishing_client_id, deadline).await;
        }

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

    async fn plan_share_groups(
        &self,
        share_groups: &HashMap<String, Vec<&Subscription>>,
        publish: &PublishPacket,
        clients: &HashMap<String, ClientInfo>,
        publishing_client_id: Option<&str>,
    ) -> Vec<DeliveryPlan> {
        let mut plans = Vec::new();
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
                    let start = counter.fetch_add(1, Ordering::Relaxed) % online_subs.len();
                    let chosen_sub = (0..online_subs.len())
                        .map(|offset| online_subs[(start + offset) % online_subs.len()])
                        .find(|sub| {
                            clients
                                .get(&sub.client_id)
                                .is_some_and(|info| !info.queue.behind())
                        })
                        .unwrap_or(online_subs[start]);
                    if let Some(plan) = self
                        .plan_delivery(chosen_sub, publish, clients, publishing_client_id)
                        .await
                    {
                        plans.push(plan);
                    }
                }
            } else if !group_subs.is_empty() {
                let sub = group_subs[0];
                if self.storage.is_some() && sub.qos != QoS::AtMostOnce {
                    let mut message = publish.clone();
                    message.qos = sub.qos;
                    plans.push(self.plan_behind(sub, message));
                }
            }
        }
        plans
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

    pub(crate) fn effective_qos(publish_qos: QoS, sub_qos: QoS) -> QoS {
        match (publish_qos, sub_qos) {
            (QoS::AtMostOnce, _) | (_, QoS::AtMostOnce) => QoS::AtMostOnce,
            (QoS::AtLeastOnce | QoS::ExactlyOnce, QoS::AtLeastOnce)
            | (QoS::AtLeastOnce, QoS::ExactlyOnce) => QoS::AtLeastOnce,
            (QoS::ExactlyOnce, QoS::ExactlyOnce) => QoS::ExactlyOnce,
        }
    }

    fn prepare_message(publish: &PublishPacket, sub: &Subscription, qos: QoS) -> PublishPacket {
        let mut message = publish.clone();
        message.packet_id = None;
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

    fn queue_behind(
        queue: &QueueHandle,
        epoch: u64,
        message: PublishPacket,
        client_id: &str,
        target_flow: Option<u64>,
    ) {
        let qos = message.qos;
        let queued = QueuedMessage::new(message, client_id.to_string(), qos, None)
            .with_target_flow(target_flow);
        let Some(outcome) = queue.push_in_epoch(queued, epoch) else {
            debug!(
                client_id,
                "Dropped message routed to a session that has since been discarded"
            );
            return;
        };
        queue.notify();
        trace!(
            client_id,
            seq = outcome.seq,
            dropped = outcome.dropped_oldest,
            "Queued message behind the client's backlog"
        );
    }

    async fn execute_plan(
        plan: DeliveryPlan,
        publishing_client_id: Option<&str>,
        deadline: Option<Instant>,
    ) {
        match plan {
            DeliveryPlan::Behind {
                client_id,
                message,
                target_flow,
                queue,
                epoch,
            } => Self::queue_behind(&queue, epoch, message, &client_id, target_flow),
            DeliveryPlan::Online {
                client_id,
                message,
                target_flow,
                lanes,
                queue,
                epoch,
            } => {
                let routable = RoutableMessage {
                    publish: message,
                    target_flow,
                };
                if routable.publish.qos == QoS::AtMostOnce {
                    if lanes.qos0_tx.try_send(routable).is_err() {
                        trace!(client_id, "Dropped QoS 0 message: delivery lane full");
                    }
                    return;
                }
                if queue.behind() {
                    Self::queue_behind(
                        &queue,
                        epoch,
                        routable.publish,
                        &client_id,
                        routable.target_flow,
                    );
                    return;
                }
                let routable = match lanes.qos1_tx.try_send(routable) {
                    Ok(()) => return,
                    Err(mpsc::error::TrySendError::Closed(rejected)) => {
                        Self::queue_behind(
                            &queue,
                            epoch,
                            rejected.publish,
                            &client_id,
                            rejected.target_flow,
                        );
                        return;
                    }
                    Err(mpsc::error::TrySendError::Full(rejected)) => rejected,
                };
                if publishing_client_id == Some(client_id.as_str()) {
                    Self::queue_behind(
                        &queue,
                        epoch,
                        routable.publish,
                        &client_id,
                        routable.target_flow,
                    );
                    return;
                }
                let permit = match deadline {
                    Some(deadline) => tokio::time::timeout_at(deadline, lanes.qos1_tx.reserve())
                        .await
                        .ok()
                        .and_then(std::result::Result::ok),
                    None => None,
                };
                match permit {
                    Some(permit) => permit.send(routable),
                    None => Self::queue_behind(
                        &queue,
                        epoch,
                        routable.publish,
                        &client_id,
                        routable.target_flow,
                    ),
                }
            }
        }
    }

    async fn plan_delivery(
        &self,
        sub: &Subscription,
        publish: &PublishPacket,
        clients: &HashMap<String, ClientInfo>,
        publishing_client_id: Option<&str>,
    ) -> Option<DeliveryPlan> {
        if sub.no_local && publishing_client_id == Some(&sub.client_id) {
            trace!(
                "Skipping delivery to {} due to No Local flag",
                sub.client_id
            );
            return None;
        }

        {
            let suppression_key = self.echo_suppression_key.read().await;
            if let Some(ref key) = *suppression_key {
                if let Some(origin) = publish.properties.get_user_property_value(key) {
                    if origin == sub.client_id {
                        trace!("Skipping echo delivery to {}", sub.client_id);
                        return None;
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
                    return None;
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
                if qos == QoS::AtMostOnce || self.storage.is_none() {
                    return None;
                }
                return Some(self.plan_behind(sub, Self::prepare_message(publish, sub, qos)));
            }
        }

        if let Some(client_info) = clients.get(&sub.client_id) {
            let qos = Self::effective_qos(publish.qos, sub.qos);
            let message = Self::prepare_message(publish, sub, qos);
            if sub.change_only {
                let mut change_only_states = self.change_only_states.write().await;
                change_only_states
                    .entry(sub.client_id.clone())
                    .or_default()
                    .update_hash(&publish.topic_name, &publish.payload);
            }
            return Some(DeliveryPlan::Online {
                client_id: sub.client_id.clone(),
                message,
                target_flow: sub.flow_id,
                lanes: DeliveryLanes {
                    qos1_tx: client_info.qos1_tx.clone(),
                    qos0_tx: client_info.qos0_tx.clone(),
                },
                queue: Arc::clone(&client_info.queue),
                epoch: client_info.queue.epoch(),
            });
        }

        if sub.qos == QoS::AtMostOnce {
            return None;
        }
        if self.storage.is_none() {
            debug!(
                "No storage configured, cannot queue message for offline client {}",
                sub.client_id
            );
            return None;
        }
        Some(self.plan_behind(sub, Self::prepare_message(publish, sub, sub.qos)))
    }

    fn plan_behind(&self, sub: &Subscription, message: PublishPacket) -> DeliveryPlan {
        let queue = self.queue_handle(&sub.client_id);
        DeliveryPlan::Behind {
            client_id: sub.client_id.clone(),
            message,
            target_flow: sub.flow_id,
            epoch: queue.epoch(),
            queue,
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
    use crate::broker::storage::ClientSession;
    use bytes::Bytes;

    struct TestLanes {
        qos1_rx: mpsc::Receiver<RoutableMessage>,
        qos0_rx: mpsc::Receiver<RoutableMessage>,
        lanes: DeliveryLanes,
    }

    impl TestLanes {
        fn new(capacity: usize) -> Self {
            let (qos1_tx, qos1_rx) = mpsc::channel(capacity);
            let (qos0_tx, qos0_rx) = mpsc::channel(capacity);
            Self {
                qos1_rx,
                qos0_rx,
                lanes: DeliveryLanes { qos1_tx, qos0_tx },
            }
        }

        fn lanes(&self) -> DeliveryLanes {
            self.lanes.clone()
        }

        fn try_recv(&mut self) -> std::result::Result<RoutableMessage, ()> {
            self.qos1_rx
                .try_recv()
                .or_else(|_| self.qos0_rx.try_recv())
                .map_err(|_| ())
        }

        async fn recv_async(&mut self) -> std::result::Result<RoutableMessage, ()> {
            tokio::select! {
                message = self.qos1_rx.recv() => message.ok_or(()),
                message = self.qos0_rx.recv() => message.ok_or(()),
            }
        }
    }

    #[tokio::test]
    async fn test_client_registration() {
        let router = MessageRouter::new();
        let rx = TestLanes::new(100);

        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "client1".to_string(),
                rx.lanes(),
                router.queue_handle("client1"),
                dtx,
            )
            .await;
        assert_eq!(router.client_count().await, 1);

        router.unregister_client("client1").await;
        assert_eq!(router.client_count().await, 0);
    }

    #[tokio::test]
    async fn test_subscription_management() {
        let router = MessageRouter::new();
        let rx = TestLanes::new(100);

        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "client1".to_string(),
                rx.lanes(),
                router.queue_handle("client1"),
                dtx,
            )
            .await;
        router
            .subscribe(SubscriptionRequest::new(
                "client1",
                "test/+",
                QoS::AtLeastOnce,
            ))
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
        let mut rx1 = TestLanes::new(100);
        let mut rx2 = TestLanes::new(100);

        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "client1".to_string(),
                rx1.lanes(),
                router.queue_handle("client1"),
                dtx1,
            )
            .await;
        router
            .register_client(
                "client2".to_string(),
                rx2.lanes(),
                router.queue_handle("client2"),
                dtx2,
            )
            .await;

        router
            .subscribe(SubscriptionRequest::new(
                "client1",
                "test/+",
                QoS::AtLeastOnce,
            ))
            .await
            .unwrap();
        router
            .subscribe(SubscriptionRequest::new(
                "client2",
                "test/data",
                QoS::ExactlyOnce,
            ))
            .await
            .unwrap();

        let publish = PublishPacket::new("test/data", &b"hello"[..], QoS::ExactlyOnce);

        router.route_message(&publish, None).await;

        let rm1 = rx1.try_recv().unwrap();
        assert_eq!(rm1.publish.topic_name, "test/data");
        assert_eq!(rm1.publish.qos, QoS::AtLeastOnce);

        let rm2 = rx2.try_recv().unwrap();
        assert_eq!(rm2.publish.topic_name, "test/data");
        assert_eq!(rm2.publish.qos, QoS::ExactlyOnce);
    }

    #[tokio::test]
    async fn routed_publish_never_carries_the_publisher_packet_id() {
        let router = MessageRouter::new();
        let mut rx = TestLanes::new(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "sub".to_string(),
                rx.lanes(),
                router.queue_handle("sub"),
                dtx,
            )
            .await;
        router
            .subscribe(SubscriptionRequest::new("sub", "ids/#", QoS::AtLeastOnce))
            .await
            .unwrap();

        for publisher_packet_id in [7u16, 7, 9] {
            let mut publish = PublishPacket::new("ids/a", &b"x"[..], QoS::AtLeastOnce);
            publish.packet_id = Some(publisher_packet_id);
            router.route_message(&publish, Some("pub")).await;
            let delivered = rx.try_recv().unwrap();
            assert_eq!(delivered.publish.packet_id, None);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn routing_and_unregistration_never_deadlock() {
        let router = Arc::new(MessageRouter::new());
        let sub_rx = TestLanes::new(10_000);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "subscriber".to_string(),
                sub_rx.lanes(),
                router.queue_handle("subscriber"),
                dtx,
            )
            .await;
        router
            .subscribe(SubscriptionRequest::new(
                "subscriber",
                "lock/#",
                QoS::AtLeastOnce,
            ))
            .await
            .unwrap();

        let publisher = {
            let router = Arc::clone(&router);
            tokio::spawn(async move {
                let publish = PublishPacket::new("lock/step", &b"m"[..], QoS::AtLeastOnce);
                for _ in 0..2_000 {
                    router.route_message(&publish, Some("publisher")).await;
                }
            })
        };
        let churn = {
            let router = Arc::clone(&router);
            tokio::spawn(async move {
                for round in 0..2_000 {
                    let id = format!("churn-{}", round % 8);
                    let rx = TestLanes::new(1);
                    let (dtx, _drx) = tokio::sync::oneshot::channel();
                    router
                        .register_client(id.clone(), rx.lanes(), router.queue_handle(&id), dtx)
                        .await;
                    router
                        .subscribe(SubscriptionRequest::new(
                            id.clone(),
                            "lock/#",
                            QoS::AtLeastOnce,
                        ))
                        .await
                        .unwrap();
                    router.unregister_client(&id).await;
                }
            })
        };
        let cleanup = {
            let router = Arc::clone(&router);
            tokio::spawn(async move {
                for _ in 0..500 {
                    router.sweep_sessions().await;
                    tokio::task::yield_now().await;
                }
            })
        };

        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            publisher.await.unwrap();
            churn.await.unwrap();
            cleanup.await.unwrap();
        })
        .await
        .expect("router lock order must not deadlock");
    }

    #[tokio::test]
    async fn test_retained_messages() {
        let router = MessageRouter::new();

        let mut publish = PublishPacket::new("test/status", &b"online"[..], QoS::AtMostOnce);
        publish.retain = true;
        router.route_message(&publish, None).await;

        assert_eq!(router.retained_count().await, 1);

        let retained = router.get_retained_messages("test/+").await;
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].topic_name, "test/status");

        let mut delete = PublishPacket::new("test/status", &b""[..], QoS::AtMostOnce);
        delete.retain = true;
        router.route_message(&delete, None).await;

        assert_eq!(router.retained_count().await, 0);
    }

    #[tokio::test]
    async fn test_shared_subscription_round_robin() {
        let router = MessageRouter::new();
        let mut rx1 = TestLanes::new(100);
        let mut rx2 = TestLanes::new(100);
        let mut rx3 = TestLanes::new(100);

        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "client1".to_string(),
                rx1.lanes(),
                router.queue_handle("client1"),
                dtx1,
            )
            .await;
        router
            .register_client(
                "client2".to_string(),
                rx2.lanes(),
                router.queue_handle("client2"),
                dtx2,
            )
            .await;
        let (dtx3, _drx3) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "client3".to_string(),
                rx3.lanes(),
                router.queue_handle("client3"),
                dtx3,
            )
            .await;

        router
            .subscribe(SubscriptionRequest::new(
                "client1",
                "$share/workers/test/data",
                QoS::AtMostOnce,
            ))
            .await
            .unwrap();
        router
            .subscribe(SubscriptionRequest::new(
                "client2",
                "$share/workers/test/data",
                QoS::AtMostOnce,
            ))
            .await
            .unwrap();
        router
            .subscribe(SubscriptionRequest::new(
                "client3",
                "$share/workers/test/data",
                QoS::AtMostOnce,
            ))
            .await
            .unwrap();

        for i in 0..6 {
            let publish = PublishPacket::new(
                "test/data",
                Bytes::copy_from_slice(format!("msg{i}").as_bytes()),
                QoS::AtMostOnce,
            );
            router.route_message(&publish, None).await;
        }

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
        let mut rx1 = TestLanes::new(100);
        let mut rx2 = TestLanes::new(100);
        let mut rx3 = TestLanes::new(100);

        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "shared1".to_string(),
                rx1.lanes(),
                router.queue_handle("shared1"),
                dtx1,
            )
            .await;
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "shared2".to_string(),
                rx2.lanes(),
                router.queue_handle("shared2"),
                dtx2,
            )
            .await;
        let (dtx3, _drx3) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "regular".to_string(),
                rx3.lanes(),
                router.queue_handle("regular"),
                dtx3,
            )
            .await;

        router
            .subscribe(SubscriptionRequest::new(
                "shared1",
                "$share/group/test/+",
                QoS::AtMostOnce,
            ))
            .await
            .unwrap();
        router
            .subscribe(SubscriptionRequest::new(
                "shared2",
                "$share/group/test/+",
                QoS::AtMostOnce,
            ))
            .await
            .unwrap();

        router
            .subscribe(SubscriptionRequest::new(
                "regular",
                "test/+",
                QoS::AtMostOnce,
            ))
            .await
            .unwrap();

        let publish = PublishPacket::new("test/data", &b"hello"[..], QoS::AtMostOnce);
        router.route_message(&publish, None).await;

        let regular_rm = rx3.try_recv().unwrap();
        assert_eq!(&regular_rm.publish.payload[..], b"hello");

        let shared1_received = rx1.try_recv().is_ok();
        let shared2_received = rx2.try_recv().is_ok();

        assert!(shared1_received ^ shared2_received);
    }

    #[tokio::test]
    async fn test_route_message_local_only_delivers_to_subscribers() {
        let router = MessageRouter::new();
        let mut rx1 = TestLanes::new(100);
        let mut rx2 = TestLanes::new(100);

        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "client1".to_string(),
                rx1.lanes(),
                router.queue_handle("client1"),
                dtx1,
            )
            .await;
        router
            .register_client(
                "client2".to_string(),
                rx2.lanes(),
                router.queue_handle("client2"),
                dtx2,
            )
            .await;

        router
            .subscribe(SubscriptionRequest::new(
                "client1",
                "test/+",
                QoS::AtLeastOnce,
            ))
            .await
            .unwrap();
        router
            .subscribe(SubscriptionRequest::new(
                "client2",
                "test/data",
                QoS::ExactlyOnce,
            ))
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
        let mut rx1 = TestLanes::new(100);
        let mut rx2 = TestLanes::new(100);

        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "client1".to_string(),
                rx1.lanes(),
                router.queue_handle("client1"),
                dtx1,
            )
            .await;
        router
            .register_client(
                "client2".to_string(),
                rx2.lanes(),
                router.queue_handle("client2"),
                dtx2,
            )
            .await;

        router
            .subscribe(SubscriptionRequest::new(
                "client1",
                "test/echo",
                QoS::AtMostOnce,
            ))
            .await
            .unwrap();
        router
            .subscribe(SubscriptionRequest::new(
                "client2",
                "test/echo",
                QoS::AtMostOnce,
            ))
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
        let mut rx1 = TestLanes::new(100);
        let mut rx2 = TestLanes::new(100);

        let (dtx1, _drx1) = tokio::sync::oneshot::channel();
        let (dtx2, _drx2) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "client1".to_string(),
                rx1.lanes(),
                router.queue_handle("client1"),
                dtx1,
            )
            .await;
        router
            .register_client(
                "client2".to_string(),
                rx2.lanes(),
                router.queue_handle("client2"),
                dtx2,
            )
            .await;

        router
            .subscribe(SubscriptionRequest::new(
                "client1",
                "test/echo",
                QoS::AtMostOnce,
            ))
            .await
            .unwrap();
        router
            .subscribe(SubscriptionRequest::new(
                "client2",
                "test/echo",
                QoS::AtMostOnce,
            ))
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
        let mut rx = TestLanes::new(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "sub1".to_string(),
                rx.lanes(),
                router.queue_handle("sub1"),
                dtx,
            )
            .await;
        router
            .subscribe(SubscriptionRequest::new(
                "sub1",
                "test/rate",
                QoS::AtMostOnce,
            ))
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
        let mut rx = TestLanes::new(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "sub1".to_string(),
                rx.lanes(),
                router.queue_handle("sub1"),
                dtx,
            )
            .await;
        router
            .subscribe(SubscriptionRequest::new(
                "sub1",
                "test/rate",
                QoS::AtLeastOnce,
            ))
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
        let mut rx = TestLanes::new(1000);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "sub1".to_string(),
                rx.lanes(),
                router.queue_handle("sub1"),
                dtx,
            )
            .await;
        router
            .subscribe(SubscriptionRequest::new(
                "sub1",
                "test/rate",
                QoS::AtMostOnce,
            ))
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
        let rx = TestLanes::new(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "sub1".to_string(),
                rx.lanes(),
                router.queue_handle("sub1"),
                dtx,
            )
            .await;
        assert!(router.outbound_rates.read().contains_key("sub1"));

        router.unregister_client("sub1").await;
        assert!(!router.outbound_rates.read().contains_key("sub1"));
    }

    #[tokio::test]
    async fn test_flow_bound_subscription_delivers_with_target_flow() {
        let router = MessageRouter::new();
        let mut rx = TestLanes::new(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client("c1".to_string(), rx.lanes(), router.queue_handle("c1"), dtx)
            .await;

        router
            .subscribe(
                SubscriptionRequest::new("c1", "sensor/#", QoS::AtLeastOnce).with_flow_id(Some(42)),
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
        let mut rx = TestLanes::new(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client("c1".to_string(), rx.lanes(), router.queue_handle("c1"), dtx)
            .await;

        router
            .subscribe(SubscriptionRequest::new("c1", "data/#", QoS::AtMostOnce))
            .await
            .unwrap();

        router
            .subscribe(
                SubscriptionRequest::new("c1", "data/#", QoS::AtMostOnce).with_flow_id(Some(7)),
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
        let mut rx = TestLanes::new(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client("c1".to_string(), rx.lanes(), router.queue_handle("c1"), dtx)
            .await;

        router
            .subscribe(SubscriptionRequest::new("c1", "topic/a", QoS::AtMostOnce))
            .await
            .unwrap();

        router
            .subscribe(
                SubscriptionRequest::new("c1", "topic/a", QoS::AtMostOnce).with_flow_id(Some(10)),
            )
            .await
            .unwrap();

        router
            .subscribe(
                SubscriptionRequest::new("c1", "topic/b", QoS::AtMostOnce).with_flow_id(Some(10)),
            )
            .await
            .unwrap();

        let removed = router.unsubscribe_by_flow(None, "c1", 10).await;
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
        let mut rx = TestLanes::new(100);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client("c1".to_string(), rx.lanes(), router.queue_handle("c1"), dtx)
            .await;

        router
            .subscribe(
                SubscriptionRequest::new("c1", "dup/test", QoS::AtMostOnce).with_flow_id(Some(5)),
            )
            .await
            .unwrap();

        router
            .subscribe(
                SubscriptionRequest::new("c1", "dup/test", QoS::AtLeastOnce).with_flow_id(Some(5)),
            )
            .await
            .unwrap();

        let publish = PublishPacket::new("dup/test", &b"once"[..], QoS::AtMostOnce);
        router.route_message(&publish, Some("pub1")).await;

        let routable = rx.recv_async().await.unwrap();
        assert_eq!(routable.target_flow, Some(5));
        assert!(rx.try_recv().is_err());
    }

    async fn register(router: &MessageRouter, client_id: &str, lanes: &TestLanes) -> u64 {
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client(
                client_id.to_string(),
                lanes.lanes(),
                router.queue_handle(client_id),
                dtx,
            )
            .await
            .generation
    }

    async fn stall_publish_behind_full_lane(
        router: &Arc<MessageRouter>,
        client_id: &str,
        lanes: &TestLanes,
    ) -> tokio::task::JoinHandle<()> {
        register(router, client_id, lanes).await;
        router
            .subscribe(SubscriptionRequest::new(client_id, "t", QoS::AtLeastOnce))
            .await
            .unwrap();
        let first = PublishPacket::new("t", &b"first"[..], QoS::AtLeastOnce);
        router.route_message(&first, None).await;

        let stalled = Arc::clone(router);
        let handle = tokio::spawn(async move {
            let second = PublishPacket::new("t", &b"second"[..], QoS::AtLeastOnce);
            stalled
                .route_message_with_deadline(
                    &second,
                    None,
                    Instant::now() + Duration::from_secs(30),
                )
                .await;
        });
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(
            !handle.is_finished(),
            "the publish must wait on the full lane"
        );
        handle
    }

    #[tokio::test]
    async fn clean_start_drops_a_publish_planned_for_the_previous_session() {
        let router = Arc::new(MessageRouter::new());
        let previous = TestLanes::new(1);
        let stalled = stall_publish_behind_full_lane(&router, "c", &previous).await;

        let current = TestLanes::new(1);
        let queue = router.queue_handle("c");
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_session(
                "c".to_string(),
                current.lanes(),
                Arc::clone(&queue),
                dtx,
                true,
            )
            .await;
        queue.clear(None);
        drop(previous);
        stalled.await.unwrap();

        assert_eq!(
            queue.count(),
            0,
            "a publish routed under the previous session must not reach the clean session"
        );
    }

    #[tokio::test]
    async fn resumed_session_keeps_a_publish_planned_for_the_previous_connection() {
        let router = Arc::new(MessageRouter::new());
        let previous = TestLanes::new(1);
        let stalled = stall_publish_behind_full_lane(&router, "c", &previous).await;

        let current = TestLanes::new(1);
        let queue = router.queue_handle("c");
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_session(
                "c".to_string(),
                current.lanes(),
                Arc::clone(&queue),
                dtx,
                false,
            )
            .await;
        drop(previous);
        stalled.await.unwrap();

        assert_eq!(queue.count(), 1, "a resumed session must still receive it");
    }

    #[tokio::test]
    async fn armed_will_is_claimed_exactly_once() {
        let router = MessageRouter::new();
        let lanes = TestLanes::new(10);
        let generation = register(&router, "w1", &lanes).await;

        let cancelled = router.arm_will("w1", generation).await;
        assert!(cancelled.is_some());
        router.release_client("w1", generation, true).await;

        assert!(router.claim_will("w1", generation).await);
        assert!(!router.claim_will("w1", generation).await);
        assert!(router.pending_wills.lock().is_empty());
    }

    #[tokio::test]
    async fn new_connection_cancels_armed_will() {
        let router = MessageRouter::new();
        let lanes = TestLanes::new(10);
        let generation = register(&router, "w2", &lanes).await;
        let cancelled = router
            .arm_will("w2", generation)
            .await
            .expect("owner can arm its will");
        router.release_client("w2", generation, true).await;

        register(&router, "w2", &lanes).await;

        let woken = tokio::time::timeout(Duration::from_secs(5), cancelled).await;
        assert!(
            matches!(woken, Ok(Ok(()))),
            "the timer is woken by the cancel"
        );
        assert!(!router.claim_will("w2", generation).await);
        assert!(router.pending_wills.lock().is_empty());
    }

    #[tokio::test]
    async fn claim_rejects_a_will_armed_by_an_older_connection() {
        let router = MessageRouter::new();
        let lanes = TestLanes::new(10);
        let first = register(&router, "w5", &lanes).await;
        let first_armed = router.arm_will("w5", first).await;
        assert!(first_armed.is_some());
        router.release_client("w5", first, true).await;

        let second = register(&router, "w5", &lanes).await;
        router.release_client("w5", second, true).await;
        let second_armed = router.arm_will("w5", second).await;
        assert!(second_armed.is_none(), "a released connection cannot arm");

        let third = register(&router, "w5", &lanes).await;
        let third_armed = router.arm_will("w5", third).await;
        assert!(third_armed.is_some());
        router.release_client("w5", third, true).await;

        assert!(
            !router.claim_will("w5", first).await,
            "the first connection's will was cancelled; its timer must not claim the newer will"
        );
        assert!(router.claim_will("w5", third).await);
        assert!(router.pending_wills.lock().is_empty());
    }

    #[tokio::test]
    async fn displaced_connection_cannot_arm_will() {
        let router = MessageRouter::new();
        let lanes = TestLanes::new(10);
        let displaced = register(&router, "w3", &lanes).await;
        register(&router, "w3", &lanes).await;

        assert!(router.arm_will("w3", displaced).await.is_none());
        assert!(router.pending_wills.lock().is_empty());
    }

    fn session_with_will(client_id: &str, connection_token: u64) -> ClientSession {
        let will = crate::types::WillMessage::new(format!("will/{client_id}"), "offline");
        let mut session = ClientSession::new_with_will(client_id, true, Some(60), Some(will));
        session.connection_token = connection_token;
        session
    }

    #[tokio::test]
    async fn claimed_will_is_removed_from_stored_session() {
        let storage = Arc::new(DynamicStorage::Memory(
            crate::broker::storage::MemoryBackend::new(),
        ));
        let router = MessageRouter::with_storage(Arc::clone(&storage));
        let lanes = TestLanes::new(10);
        let generation = register(&router, "w4", &lanes).await;
        storage
            .store_session(session_with_will("w4", generation))
            .await
            .unwrap();
        let armed = router.arm_will("w4", generation).await;
        assert!(armed.is_some());
        router.release_client("w4", generation, true).await;

        assert!(router.claim_will("w4", generation).await);

        let stored = storage.get_session("w4").await.unwrap().unwrap();
        assert!(stored.will_message.is_none());
        assert!(stored.will_delay_interval.is_none());
    }

    #[tokio::test]
    async fn claimed_will_leaves_a_newer_stored_session_alone() {
        let storage = Arc::new(DynamicStorage::Memory(
            crate::broker::storage::MemoryBackend::new(),
        ));
        let router = MessageRouter::with_storage(Arc::clone(&storage));
        let lanes = TestLanes::new(10);
        let generation = register(&router, "w6", &lanes).await;
        storage
            .store_session(session_with_will("w6", generation))
            .await
            .unwrap();
        let armed = router.arm_will("w6", generation).await;
        assert!(armed.is_some());
        router.release_client("w6", generation, true).await;

        let successor = register(&router, "w6", &lanes).await;
        storage
            .store_session(session_with_will("w6", successor))
            .await
            .unwrap();

        assert!(!router.claim_will("w6", generation).await);

        let stored = storage.get_session("w6").await.unwrap().unwrap();
        assert_eq!(stored.connection_token, successor);
        assert!(
            stored.will_message.is_some(),
            "the successor's freshly stored will must survive"
        );
    }

    #[tokio::test]
    async fn generations_start_above_every_earlier_run() {
        let earlier = MessageRouter::new();
        let lanes = TestLanes::new(10);
        let earlier_generation = register(&earlier, "epoch", &lanes).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        let later = MessageRouter::new();
        let later_generation = register(&later, "epoch", &lanes).await;
        assert!(
            later_generation > earlier_generation,
            "a restarted router must never hand out a generation a previous run used"
        );
    }

    #[tokio::test]
    async fn release_of_a_missing_entry_is_not_owned() {
        let router = MessageRouter::new();
        assert!(matches!(
            router.release_client("absent", 1, false).await,
            Release::Displaced
        ));
    }

    #[tokio::test]
    async fn sweep_leaves_a_client_id_claimed_while_it_waited_for_the_slot() {
        let router = Arc::new(MessageRouter::new());
        router
            .subscribe(SubscriptionRequest::new(
                "late-owner",
                "late/t",
                QoS::AtMostOnce,
            ))
            .await
            .unwrap();
        let slot = router.lock_session("late-owner").await;
        let sweeping = tokio::spawn({
            let router = Arc::clone(&router);
            async move { router.sweep_sessions().await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let lanes = TestLanes::new(10);
        register(&router, "late-owner", &lanes).await;
        drop(slot);
        sweeping.await.unwrap();
        assert!(
            router.has_subscription("late-owner", "late/t").await,
            "the sweep stripped the routes of a connection that claimed the ClientID while the sweep waited"
        );
    }

    #[tokio::test]
    async fn exact_subscription_set_replaces_rather_than_unions() {
        let router = MessageRouter::new();
        let rx = TestLanes::new(10);
        let (dtx, _drx) = tokio::sync::oneshot::channel();
        router
            .register_client(
                "exact".to_string(),
                rx.lanes(),
                router.queue_handle("exact"),
                dtx,
            )
            .await;
        router
            .subscribe(SubscriptionRequest::new(
                "exact",
                "old/topic",
                QoS::AtMostOnce,
            ))
            .await
            .unwrap();
        let mut session = ClientSession::new("exact", true, Some(60));
        session.add_subscription(
            "new/topic",
            StoredSubscription {
                qos: QoS::AtLeastOnce,
                no_local: false,
                retain_as_published: false,
                retain_handling: 0,
                subscription_id: None,
                protocol_version: 5,
                change_only: false,
                flow_id: None,
            },
        );
        let slot = router.lock_session("exact").await;
        router
            .set_client_subscriptions(&slot, Some(&session))
            .await
            .unwrap();
        drop(slot);
        assert!(!router.has_subscription("exact", "old/topic").await);
        assert!(router.has_subscription("exact", "new/topic").await);

        let slot = router.lock_session("exact").await;
        router.set_client_subscriptions(&slot, None).await.unwrap();
        drop(slot);
        assert_eq!(router.subscription_count_for_client("exact").await, 0);
        assert_eq!(router.session_slot_count(), 0);
    }

    fn ended_session(client_id: &str, expiry: u32, ended_ago_ms: u64) -> ClientSession {
        let mut session = ClientSession::new(client_id, true, Some(expiry));
        session.add_subscription(
            format!("{client_id}/t"),
            crate::broker::storage::StoredSubscription::new(QoS::AtLeastOnce),
        );
        session.mark_disconnected(unix_millis_now() - ended_ago_ms);
        session
    }

    #[tokio::test]
    async fn session_expired_by_the_capped_expiry_is_not_routed_after_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(DynamicStorage::File(
            crate::broker::storage::FileBackend::new(dir.path())
                .await
                .unwrap(),
        ));
        storage
            .store_session(ended_session("stale", 3600, 120_000))
            .await
            .unwrap();
        let router = MessageRouter::with_storage(Arc::clone(&storage));
        router.recover_sessions(60).await.unwrap();
        assert_eq!(
            router.subscription_count_for_client("stale").await,
            0,
            "a session that ended 120s ago outlived the 60s maximum, yet its routes were rebuilt"
        );
        assert!(storage.session_client_ids().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capped_expiry_of_an_ended_session_is_stored_at_recovery() {
        let dir = tempfile::tempdir().unwrap();
        {
            let storage = Arc::new(DynamicStorage::File(
                crate::broker::storage::FileBackend::new(dir.path())
                    .await
                    .unwrap(),
            ));
            storage
                .store_session(ended_session("idle", 3600, 10_000))
                .await
                .unwrap();
            let router = MessageRouter::with_storage(Arc::clone(&storage));
            router.recover_sessions(60).await.unwrap();
            assert_eq!(router.subscription_count_for_client("idle").await, 1);
            assert_eq!(
                storage
                    .get_session("idle")
                    .await
                    .unwrap()
                    .unwrap()
                    .expiry_interval,
                Some(60)
            );
        }
        let reopened = crate::broker::storage::FileBackend::new(dir.path())
            .await
            .unwrap();
        assert_eq!(
            reopened
                .get_session("idle")
                .await
                .unwrap()
                .unwrap()
                .expiry_interval,
            Some(60),
            "the capped expiry must be stored, not only applied in memory"
        );
    }
}
