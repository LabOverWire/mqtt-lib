//! Bridge connection implementation
//!
//! Manages a single bridge connection to a remote broker using our existing
//! `MqttClient` implementation.

use crate::broker::acl::rules::within_pattern;
use crate::broker::bridge::{BridgeConfig, BridgeError, BridgeProtocol, BridgeStats, Result};
use crate::broker::router::MessageRouter;
use crate::client::{ConnectionEvent, DisconnectReason, MqttClient};
use crate::error::MqttError;
use crate::packet::publish::PublishPacket;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::time::{Duration, Instant};
use crate::transport::tls::TlsConfig;
use crate::transport::{certificate_name, unbracketed_host};
use crate::types::ConnectOptions;
use crate::types::PublishOptions;
use crate::validation::{parse_shared_subscription, topic_matches_filter};
use crate::{AckToken, IndeterminateReason, PublishHandle, PublishOutcome, QoS, SubscribeOptions};
use bytes::Bytes;
use futures::stream::{FuturesUnordered, StreamExt};
use mqtt5_protocol::bridge::{evaluate_forwarding, TopicMappingCore};
use rand::RngExt;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use tokio::sync::{broadcast, mpsc, watch, Notify, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Bound on remote deliveries a bridge holds unacknowledged; also its Receive Maximum.
const INGRESS_LANE_CAPACITY: usize = 1024;
/// How long the remote keeps the bridge's session (and its unacknowledged messages)
/// across a reconnect.
const BRIDGE_SESSION_EXPIRY_SECS: u32 = 3600;

/// One incoming delivery from the remote: every local copy it maps to, plus the token
/// that acknowledges the remote once all of them are routed.
type IngressItem = (Vec<PublishPacket>, AckToken);

/// A message waiting in a bridge's outbound queue for its turn to be published remotely.
struct Outbound {
    topic: String,
    payload: Bytes,
    options: PublishOptions,
    arrival: u64,
}

/// A bridge's outbound messages in the order they reach it, after the messages the remote lost
/// with its session, which are published again first in the order they reached it. Adding never
/// waits: when the queue is full the oldest waiting message is dropped.
struct OutboundQueue {
    messages: Mutex<Waiting>,
    capacity: usize,
    ready: Notify,
    closed: AtomicBool,
}

#[derive(Clone, Copy)]
enum Segment {
    Lost,
    Arrived,
}

#[derive(Default)]
struct Waiting {
    lost: VecDeque<Outbound>,
    arrived: VecDeque<Outbound>,
    next_arrival: u64,
}

impl Waiting {
    fn len(&self) -> usize {
        self.lost.len() + self.arrived.len()
    }

    fn pop_oldest(&mut self) -> Option<Outbound> {
        self.pop_with_segment().map(|(outbound, _)| outbound)
    }

    fn pop_with_segment(&mut self) -> Option<(Outbound, Segment)> {
        self.lost
            .pop_front()
            .map(|outbound| (outbound, Segment::Lost))
            .or_else(|| {
                self.arrived
                    .pop_front()
                    .map(|outbound| (outbound, Segment::Arrived))
            })
    }
}

impl OutboundQueue {
    fn new(capacity: usize) -> Self {
        Self {
            messages: Mutex::new(Waiting::default()),
            capacity,
            ready: Notify::new(),
            closed: AtomicBool::new(false),
        }
    }

    /// Returns the message dropped to make room, if any.
    fn push_back(
        &self,
        topic: String,
        payload: Bytes,
        options: PublishOptions,
    ) -> Option<Outbound> {
        let dropped = {
            let mut messages = self.messages.lock().unwrap_or_else(PoisonError::into_inner);
            let dropped = (messages.len() >= self.capacity)
                .then(|| messages.pop_oldest())
                .flatten();
            let arrival = messages.next_arrival;
            messages.next_arrival += 1;
            messages.arrived.push_back(Outbound {
                topic,
                payload,
                options,
                arrival,
            });
            dropped
        };
        self.ready.notify_one();
        dropped
    }

    fn push_lost(&self, outbounds: Vec<Outbound>) -> Vec<Outbound> {
        let dropped = {
            let mut messages = self.messages.lock().unwrap_or_else(PoisonError::into_inner);
            for outbound in outbounds {
                let at = messages
                    .lost
                    .partition_point(|waiting| waiting.arrival < outbound.arrival);
                messages.lost.insert(at, outbound);
            }
            let excess = messages.len().saturating_sub(self.capacity);
            (0..excess)
                .filter_map(|_| messages.pop_oldest())
                .collect::<Vec<_>>()
        };
        self.ready.notify_one();
        dropped
    }

    fn push_front(&self, outbound: Outbound, segment: Segment) -> Option<Outbound> {
        let mut messages = self.messages.lock().unwrap_or_else(PoisonError::into_inner);
        if messages.len() >= self.capacity {
            return Some(outbound);
        }
        match segment {
            Segment::Lost => {
                let at = messages
                    .lost
                    .partition_point(|waiting| waiting.arrival < outbound.arrival);
                messages.lost.insert(at, outbound);
            }
            Segment::Arrived => messages.arrived.push_front(outbound),
        }
        None
    }

    /// Returns `None` once the queue is closed.
    async fn pop_front(&self) -> Option<(Outbound, Segment)> {
        loop {
            if self.closed.load(Ordering::Acquire) {
                return None;
            }
            let next = self
                .messages
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_with_segment();
            if next.is_some() {
                return next;
            }
            self.ready.notified().await;
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.ready.notify_one();
    }
}

/// The single task that publishes a bridge's outbound queue to the remote broker in order.
struct Egress {
    client: Weak<MqttClient>,
    queue: Arc<OutboundQueue>,
    unsettled: Arc<Unsettled>,
    messages_sent: Arc<AtomicU64>,
    bytes_sent: Arc<AtomicU64>,
    bridge_name: String,
}

/// The `QoS` 1 and 2 publishes handed to the client whose outcome is not known yet, in the
/// order they were sent.
struct Unsettled {
    publishes: Mutex<BTreeMap<u64, (Outbound, PublishHandle)>>,
    queue: Arc<OutboundQueue>,
    bridge_name: String,
}

impl Unsettled {
    /// Puts the `QoS` 2 publishes the remote lost with its session back ahead of the queued
    /// messages, in the order they were sent, up to the first `QoS` 2 publish still pending.
    /// Returns whether one is still pending.
    fn requeue_lost(&self) -> bool {
        let mut publishes = self
            .publishes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut lost = Vec::new();
        let mut blocked = false;
        for (sequence, (outbound, handle)) in publishes.iter() {
            match handle.try_outcome() {
                Some(PublishOutcome::Indeterminate(IndeterminateReason::SessionLost)) => {
                    lost.push(*sequence);
                }
                None if outbound.options.qos == QoS::ExactlyOnce => {
                    blocked = true;
                    break;
                }
                _ => {}
            }
        }
        let requeued = lost
            .into_iter()
            .filter_map(|sequence| publishes.remove(&sequence).map(|(outbound, _)| outbound))
            .collect();
        drop(publishes);
        for dropped in self.queue.push_lost(requeued) {
            warn!(bridge = %self.bridge_name, topic = %dropped.topic, "outbound queue full, dropped the oldest message");
        }
        blocked
    }
}

const EGRESS_RETRY_DELAY: Duration = Duration::from_millis(50);
const EGRESS_UNSETTLED_CAPACITY: usize = u16::MAX as usize;

enum Begun {
    Handle(PublishHandle),
    LinkDown,
    Abandoned,
}

impl Egress {
    /// Hands queued messages to the client only while the bridge is connected, so while it
    /// is not they wait in the outbound queue, under its capacity, in order.
    async fn run(self, mut link: watch::Receiver<bool>) {
        let egress = Arc::new(self);
        let (unsettled, settling) =
            mpsc::channel::<(Outbound, PublishHandle)>(EGRESS_UNSETTLED_CAPACITY);
        let settler = Arc::clone(&egress);
        tokio::spawn(async move { settler.settle_all(settling).await });
        loop {
            if link.wait_for(|up| *up).await.is_err() {
                return;
            }
            let Some((outbound, segment)) = egress.queue.pop_front().await else {
                return;
            };
            match egress.begin(&outbound, &link).await {
                Begun::Handle(handle) if outbound.options.qos == QoS::AtMostOnce => {
                    egress.settle(outbound, handle).await;
                }
                Begun::Handle(handle) => {
                    if unsettled.send((outbound, handle)).await.is_err() {
                        return;
                    }
                }
                Begun::LinkDown => {
                    if let Some(dropped) = egress.queue.push_front(outbound, segment) {
                        warn!(bridge = %egress.bridge_name, topic = %dropped.topic, "outbound queue full, dropped the oldest message");
                    }
                }
                Begun::Abandoned => {}
            }
        }
    }

    /// Hands the message to the client while the bridge is connected. A message refused because
    /// the client is not connected, or whose wait for send quota ends without it being written,
    /// is tried again while the bridge stays connected, so the messages behind it wait; once
    /// the bridge has disconnected, even if it has connected again since, it goes back to the
    /// front of the queue, behind any messages the remote lost with its session.
    async fn begin(&self, outbound: &Outbound, link: &watch::Receiver<bool>) -> Begun {
        loop {
            if !*link.borrow() || link.has_changed().unwrap_or(true) {
                return Begun::LinkDown;
            }
            let Some(live) = self.client.upgrade() else {
                debug!(bridge = %self.bridge_name, topic = %outbound.topic, "publish abandoned: bridge client dropped");
                return Begun::Abandoned;
            };
            let begun = live
                .begin_publish(
                    outbound.topic.as_str(),
                    outbound.payload.to_vec(),
                    outbound.options.clone(),
                )
                .await;
            drop(live);
            match begun {
                Ok(handle) => return Begun::Handle(handle),
                Err(
                    MqttError::NotConnected
                    | MqttError::FlowControlExceeded
                    | MqttError::PacketIdExhausted,
                ) => {
                    tokio::time::sleep(EGRESS_RETRY_DELAY).await;
                }
                Err(e) => {
                    warn!(bridge = %self.bridge_name, topic = %outbound.topic, error = %e, "publish not sent");
                    return Begun::Abandoned;
                }
            }
        }
    }

    async fn settle_all(&self, mut settling: mpsc::Receiver<(Outbound, PublishHandle)>) {
        let mut resolving = FuturesUnordered::new();
        let mut next_sequence = 0u64;
        let mut open = true;
        let mut lost_waiting = false;
        while open || !resolving.is_empty() {
            tokio::select! {
                item = settling.recv(), if open => match item {
                    Some((outbound, handle)) => {
                        let sequence = next_sequence;
                        next_sequence += 1;
                        let resolved = handle.clone();
                        resolving.push(async move { (sequence, resolved.outcome().await) });
                        self.unsettled
                            .publishes
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(sequence, (outbound, handle));
                    }
                    None => open = false,
                },
                Some((sequence, outcome)) = resolving.next(), if !resolving.is_empty() => {
                    if outcome == PublishOutcome::Indeterminate(IndeterminateReason::SessionLost) {
                        lost_waiting = true;
                    } else {
                        let settled = self
                            .unsettled
                            .publishes
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .remove(&sequence);
                        if let Some((outbound, _)) = settled {
                            self.record(&outbound, outcome);
                        }
                    }
                    if lost_waiting {
                        lost_waiting = self.unsettled.requeue_lost();
                    }
                }
            }
        }
    }

    fn record(&self, outbound: &Outbound, outcome: PublishOutcome) {
        match outcome {
            PublishOutcome::Delivered(_) => {
                self.messages_sent.fetch_add(1, Ordering::Relaxed);
                self.bytes_sent
                    .fetch_add(outbound.payload.len() as u64, Ordering::Relaxed);
            }
            outcome => {
                warn!(bridge = %self.bridge_name, topic = %outbound.topic, outcome = ?outcome, "publish not delivered");
            }
        }
    }

    async fn settle(&self, outbound: Outbound, handle: PublishHandle) {
        self.record(&outbound, handle.outcome().await);
    }
}

/// Delivery lanes from the remote-facing client into the local router, mirroring the
/// client handler's gated and ungated lanes.
struct Ingress {
    qos1_tx: mpsc::Sender<IngressItem>,
    qos0_tx: mpsc::Sender<Vec<PublishPacket>>,
}

/// All incoming mappings that share one remote filter, subscribed once with a
/// subscription identifier so overlapping filters route each copy exactly once.
struct IngressFilter {
    remote_filter: String,
    qos: QoS,
    mappings: Vec<TopicMappingCore>,
}

/// What this process asked of each remote broker: the filters it subscribed there, and the
/// mapping filters whose retained messages that broker has delivered.
#[derive(Default)]
struct RemoteSubscriptions {
    held: Vec<(ConnectedBroker, String)>,
    retained_delivered: Vec<(ConnectedBroker, String)>,
}

impl RemoteSubscriptions {
    fn holds(&self, broker: ConnectedBroker, filter: &str) -> bool {
        self.held
            .iter()
            .any(|(on, held)| *on == broker && held == filter)
    }

    fn retained_delivered_to(&self, broker: ConnectedBroker, filter: &str) -> bool {
        self.retained_delivered
            .iter()
            .any(|(on, delivered)| *on == broker && delivered == filter)
    }

    /// Asks for retained messages only when one of the mapping filters a subscription reaches
    /// has not had them from this broker, or when the broker held no session.
    fn retain_handling(
        &self,
        broker: ConnectedBroker,
        filter: &str,
        reaches: &[String],
        session_present: bool,
    ) -> crate::RetainHandling {
        let undelivered = reaches
            .iter()
            .any(|reached| !self.retained_delivered_to(broker, reached));
        match (session_present, undelivered, self.holds(broker, filter)) {
            (false, _, _) | (true, false, true) => crate::RetainHandling::SendIfNew,
            (true, true, _) => crate::RetainHandling::SendAtSubscribe,
            (true, false, false) => crate::RetainHandling::DontSend,
        }
    }

    fn subscribed(
        &mut self,
        broker: ConnectedBroker,
        filter: &str,
        reaches: &[String],
        retain_handling: crate::RetainHandling,
        session_present: bool,
    ) {
        if !self.holds(broker, filter) {
            self.held.push((broker, filter.to_string()));
        }
        let delivers =
            !session_present || matches!(retain_handling, crate::RetainHandling::SendAtSubscribe);
        for reached in reaches {
            if delivers && !self.retained_delivered_to(broker, reached) {
                self.retained_delivered.push((broker, reached.clone()));
            }
        }
    }

    fn unsubscribed(&mut self, broker: ConnectedBroker, filter: &str) {
        self.held
            .retain(|(on, held)| !(*on == broker && held == filter));
    }
}

/// The mapping filters whose messages a subscription to `filter` delivers: only its own with
/// identifiers, and every filter it covers without them, since those are routed by topic.
fn reached_filters(filter: &str, filters: &[IngressFilter], with_identifiers: bool) -> Vec<String> {
    filters
        .iter()
        .map(|mapped| mapped.remote_filter.clone())
        .filter(|mapped| mapped == filter || (!with_identifiers && covers(mapped, filter)))
        .collect()
}

fn is_shared(filter: &str) -> bool {
    filter.starts_with("$share/")
}

fn covers(filter: &str, by: &str) -> bool {
    !is_shared(filter) && !is_shared(by) && within_pattern(filter, by)
}

fn filters_overlap(first: &str, second: &str) -> bool {
    let (first, _) = parse_shared_subscription(first);
    let (second, _) = parse_shared_subscription(second);
    let wildcard_start = |filter: &str| filter.starts_with(['+', '#']);
    if (first.starts_with('$') && wildcard_start(second))
        || (second.starts_with('$') && wildcard_start(first))
    {
        return false;
    }
    let mut first_levels = first.split('/');
    let mut second_levels = second.split('/');
    loop {
        match (first_levels.next(), second_levels.next()) {
            (Some("#"), _) | (_, Some("#")) | (None, None) => return true,
            (Some(left), Some(right)) if left == right || left == "+" || right == "+" => {}
            _ => return false,
        }
    }
}

fn subscriptions_without_identifiers(filters: &[IngressFilter]) -> Vec<(String, QoS)> {
    filters
        .iter()
        .enumerate()
        .filter(|(index, filter)| {
            !filters.iter().enumerate().any(|(other, by)| {
                other != *index
                    && covers(&filter.remote_filter, &by.remote_filter)
                    && (other < *index || !covers(&by.remote_filter, &filter.remote_filter))
            })
        })
        .map(|(_, kept)| {
            let qos = filters
                .iter()
                .filter(|covered| {
                    covered.remote_filter == kept.remote_filter
                        || covers(&covered.remote_filter, &kept.remote_filter)
                })
                .map(|covered| covered.qos)
                .max_by_key(|qos| *qos as u8)
                .unwrap_or(kept.qos);
            (kept.remote_filter.clone(), qos)
        })
        .collect()
}

/// Connect options for the remote-facing client: a persistent deferred-ack session whose
/// receive maximum equals the ingress lane, so the remote never sends more than the bridge
/// can hold unacknowledged. The bridge reconnects by its own loop, so the client doesn't.
fn connect_options(config: &BridgeConfig) -> ConnectOptions {
    let receive_maximum = u16::try_from(INGRESS_LANE_CAPACITY).unwrap_or(u16::MAX);
    let mut options = ConnectOptions::new(&config.client_id)
        .with_automatic_reconnect(false)
        .with_deferred_ack(true)
        .with_clean_start(false)
        .with_resume_existing_session(true)
        .with_session_expiry_interval(BRIDGE_SESSION_EXPIRY_SECS)
        .with_receive_maximum(receive_maximum);
    options.keep_alive = Duration::from_secs(u64::from(config.keepalive));

    if let Some(ref username) = config.username {
        options.username = Some(username.clone());
    }
    if let Some(ref password) = config.password {
        options.password = Some(password.clone().into_bytes());
    }

    if config.try_private {
        options
            .properties
            .user_properties
            .push(("bridge".to_string(), config.name.clone()));
    }

    options
}

fn apply_jitter(delay: Duration, enable: bool) -> Duration {
    if !enable {
        return delay;
    }
    let secs = delay.as_secs_f64();
    let jitter_range = secs * 0.25;
    let jitter = (rand::rng().random::<f64>() - 0.5) * 2.0 * jitter_range;
    Duration::try_from_secs_f64((secs + jitter).max(0.1)).unwrap_or(delay)
}

fn fallback_protocols(config: &BridgeConfig) -> Vec<BridgeProtocol> {
    let tcp_fallback =
        config.fallback_tcp && !config.fallback_protocols.contains(&BridgeProtocol::Tcp);
    config
        .fallback_protocols
        .iter()
        .copied()
        .chain(tcp_fallback.then_some(BridgeProtocol::Tcp))
        .collect()
}

/// Which broker the bridge is currently connected to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectedBroker {
    /// Connected to the primary broker
    Primary,
    /// Connected to a backup broker (index in `backup_brokers` list)
    Backup(usize),
}

/// A bridge connection to a remote broker
pub struct BridgeConnection {
    /// Bridge configuration
    config: BridgeConfig,
    /// MQTT client for remote connection
    client: Arc<MqttClient>,
    /// Local message router
    router: Arc<MessageRouter>,
    /// Bridge statistics
    stats: Arc<RwLock<BridgeStats>>,
    /// Whether the bridge is running
    running: Arc<AtomicBool>,
    /// Shutdown signal
    shutdown_tx: broadcast::Sender<()>,
    /// Raised by the health check when the primary broker is back, to leave the backup
    failback: Arc<Notify>,
    /// Whether the connection being made follows a failback, so it should reach the primary
    failing_back: AtomicBool,
    /// Failbacks in a row that ended on a backup again, each doubling the wait before the next
    failed_failbacks: AtomicU32,
    /// Message counters for stats
    messages_sent: Arc<AtomicU64>,
    messages_received: Arc<AtomicU64>,
    bytes_sent: Arc<AtomicU64>,
    bytes_received: Arc<AtomicU64>,
    /// Which broker we're currently connected to
    current_broker: Arc<RwLock<Option<ConnectedBroker>>>,
    /// Handle to the health check task (runs when on backup)
    health_check_handle: Arc<RwLock<Option<JoinHandle<()>>>>,
    /// Lanes from the remote-facing client into the local router, created on first use
    ingress: OnceLock<Ingress>,
    egress: OnceLock<Arc<OutboundQueue>>,
    unsettled: OnceLock<Arc<Unsettled>>,
    link: watch::Sender<bool>,
    subscribed: tokio::sync::Mutex<RemoteSubscriptions>,
    session_present: AtomicBool,
    server_disconnect: Arc<Mutex<Option<ReasonCode>>>,
    watching_disconnects: AtomicBool,
}

impl Drop for BridgeConnection {
    fn drop(&mut self) {
        if let Some(queue) = self.egress.get() {
            queue.close();
        }
    }
}

impl BridgeConnection {
    /// Creates a new bridge connection.
    ///
    /// # Errors
    /// Returns an error if the configuration is invalid.
    pub fn new(config: BridgeConfig, router: Arc<MessageRouter>) -> Result<Self> {
        config
            .validate()
            .map_err(|e| BridgeError::ConfigurationError(e.to_string()))?;

        let client = Arc::new(MqttClient::with_options(connect_options(&config)));

        let (shutdown_tx, _) = broadcast::channel(1);

        Ok(Self {
            config,
            client,
            router,
            stats: Arc::new(RwLock::new(BridgeStats::default())),
            running: Arc::new(AtomicBool::new(false)),
            shutdown_tx,
            failback: Arc::new(Notify::new()),
            failing_back: AtomicBool::new(false),
            failed_failbacks: AtomicU32::new(0),
            messages_sent: Arc::new(AtomicU64::new(0)),
            messages_received: Arc::new(AtomicU64::new(0)),
            bytes_sent: Arc::new(AtomicU64::new(0)),
            bytes_received: Arc::new(AtomicU64::new(0)),
            current_broker: Arc::new(RwLock::new(None)),
            health_check_handle: Arc::new(RwLock::new(None)),
            ingress: OnceLock::new(),
            egress: OnceLock::new(),
            unsettled: OnceLock::new(),
            link: watch::Sender::new(false),
            subscribed: tokio::sync::Mutex::new(RemoteSubscriptions::default()),
            session_present: AtomicBool::new(false),
            server_disconnect: Arc::new(Mutex::new(None)),
            watching_disconnects: AtomicBool::new(false),
        })
    }

    /// Starts the bridge connection.
    pub fn start(&self) {
        if self.running.load(Ordering::Relaxed) {
            return;
        }

        self.failing_back.store(false, Ordering::Relaxed);
        self.failed_failbacks.store(0, Ordering::Relaxed);
        self.running.store(true, Ordering::Relaxed);
        info!("Starting bridge '{}'", self.config.name);
    }

    /// Stops the bridge connection.
    ///
    /// # Errors
    /// Returns an error if the stats update fails.
    pub async fn stop(&self) -> Result<()> {
        if !self.running.load(Ordering::Relaxed) {
            return Ok(());
        }

        info!("Stopping bridge '{}'", self.config.name);
        self.running.store(false, Ordering::Relaxed);

        self.stop_health_check().await;

        let _ = self.shutdown_tx.send(());

        if let Err(e) = self.client.disconnect().await {
            debug!(bridge = %self.config.name, error = %e, "bridge client disconnect failed during stop");
        }

        let mut stats = self.stats.write().await;
        stats.connected = false;
        self.link.send_replace(false);
        stats.connected_since = None;
        *self.current_broker.write().await = None;

        Ok(())
    }

    async fn resolve(address: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
        let host_and_port = address
            .rsplit_once(':')
            .and_then(|(host, port)| port.parse::<u16>().ok().map(|port| (host, port)));
        match host_and_port {
            Some((host, port)) => Ok(tokio::net::lookup_host((unbracketed_host(host), port))
                .await?
                .collect()),
            None => Ok(tokio::net::lookup_host(address).await?.collect()),
        }
    }

    /// Builds TLS configuration for a broker address
    fn build_tls_config(&self, address: &str, addr: std::net::SocketAddr) -> Result<TlsConfig> {
        let hostname = if let Some(ref server_name) = self.config.tls_server_name {
            server_name.clone()
        } else {
            let host = address.rsplit_once(':').map_or(address, |(host, _)| host);
            certificate_name(host).to_string()
        };

        let mut tls_config = TlsConfig::new(addr, hostname);

        if let Some(ref ca_file) = self.config.ca_file {
            tls_config.load_ca_cert_pem(ca_file).map_err(|e| {
                BridgeError::ConfigurationError(format!("Failed to load CA cert: {e}"))
            })?;
        }

        if let Some(ref cert_file) = self.config.client_cert_file {
            tls_config.load_client_cert_pem(cert_file).map_err(|e| {
                BridgeError::ConfigurationError(format!("Failed to load client cert: {e}"))
            })?;
        }

        if let Some(ref key_file) = self.config.client_key_file {
            tls_config.load_client_key_pem(key_file).map_err(|e| {
                BridgeError::ConfigurationError(format!("Failed to load client key: {e}"))
            })?;
        }

        if let Some(insecure) = self.config.insecure {
            tls_config = tls_config.with_verify_server_cert(!insecure);
        }

        if let Some(ref alpn_protocols) = self.config.alpn_protocols {
            let protocols: Vec<&str> = alpn_protocols.iter().map(String::as_str).collect();
            tls_config = tls_config.with_alpn_protocols(&protocols);
        }

        Ok(tls_config)
    }

    async fn configure_quic_tls(&self) -> Result<()> {
        let cert_pem = self
            .config
            .client_cert_file
            .as_ref()
            .map(|cert_file| {
                std::fs::read(cert_file).map_err(|e| {
                    BridgeError::ConfigurationError(format!(
                        "Failed to read client cert file {cert_file}: {e}"
                    ))
                })
            })
            .transpose()?;

        let key_pem = self
            .config
            .client_key_file
            .as_ref()
            .map(|key_file| {
                std::fs::read(key_file).map_err(|e| {
                    BridgeError::ConfigurationError(format!(
                        "Failed to read client key file {key_file}: {e}"
                    ))
                })
            })
            .transpose()?;

        let ca_pem = self
            .config
            .ca_file
            .as_ref()
            .map(|ca_file| {
                std::fs::read(ca_file).map_err(|e| {
                    BridgeError::ConfigurationError(format!(
                        "Failed to read CA cert file {ca_file}: {e}"
                    ))
                })
            })
            .transpose()?;

        if cert_pem.is_some() || key_pem.is_some() || ca_pem.is_some() {
            self.client
                .set_tls_config(cert_pem, key_pem, ca_pem)
                .await
                .map_err(|e| {
                    BridgeError::ConfigurationError(format!("Failed to load TLS certificates: {e}"))
                })?;
        }

        Ok(())
    }

    /// Builds connection options from config
    fn build_connect_options(&self) -> ConnectOptions {
        connect_options(&self.config)
    }

    /// Attempts to connect using TLS to primary and backup brokers
    async fn connect_tls(&self, options: &ConnectOptions) -> Result<ConnectedBroker> {
        match self
            .connect_tls_to(&self.config.remote_address, options)
            .await
        {
            Ok(()) => {
                info!(
                    "Bridge '{}' connected to primary broker: {} (TLS)",
                    self.config.name, self.config.remote_address
                );
                self.update_connected_stats(ConnectedBroker::Primary, &self.config.remote_address)
                    .await;
                return Ok(ConnectedBroker::Primary);
            }
            Err(e) => {
                warn!("Failed to connect to primary broker: {e}");
                self.update_error_stats(e).await;
            }
        }

        for (idx, backup) in self.config.backup_brokers.iter().enumerate() {
            match self.connect_tls_to(backup, options).await {
                Ok(()) => {
                    info!(
                        "Bridge '{}' connected to backup broker: {} (TLS)",
                        self.config.name, backup
                    );
                    self.update_connected_stats(ConnectedBroker::Backup(idx), backup)
                        .await;
                    return Ok(ConnectedBroker::Backup(idx));
                }
                Err(e) => {
                    warn!("Failed to connect to backup broker {}: {}", backup, e);
                    self.update_error_stats(e).await;
                }
            }
        }

        Err(BridgeError::ConnectionFailed(
            "Failed to connect to any broker".to_string(),
        ))
    }

    async fn connect_tls_to(
        &self,
        address: &str,
        options: &ConnectOptions,
    ) -> std::result::Result<(), String> {
        let addrs = Self::resolve(address)
            .await
            .map_err(|e| format!("Invalid address: {e}"))?;
        let mut last_error = "Could not resolve address".to_string();
        for addr in addrs {
            let tls_config = self
                .build_tls_config(address, addr)
                .map_err(|e| e.to_string())?;
            match self
                .client
                .connect_with_tls_and_options(tls_config, options.clone())
                .await
            {
                Ok(result) => {
                    self.session_present
                        .store(result.session_present, Ordering::Relaxed);
                    return Ok(());
                }
                Err(e) => last_error = e.to_string(),
            }
        }
        Err(last_error)
    }

    /// Attempts to connect without TLS to primary and backup brokers
    async fn connect_plain(&self, options: &ConnectOptions) -> Result<ConnectedBroker> {
        let connection_string = format!("mqtt://{}", self.config.remote_address);
        match Box::pin(
            self.client
                .connect_with_options(&connection_string, options.clone()),
        )
        .await
        {
            Ok(result) => {
                self.session_present
                    .store(result.session_present, Ordering::Relaxed);
                info!(
                    "Bridge '{}' connected to primary broker: {}",
                    self.config.name, self.config.remote_address
                );
                self.update_connected_stats(ConnectedBroker::Primary, &self.config.remote_address)
                    .await;
                return Ok(ConnectedBroker::Primary);
            }
            Err(e) => {
                warn!("Failed to connect to primary broker: {e}");
                self.update_error_stats(e.to_string()).await;
            }
        }

        for (idx, backup) in self.config.backup_brokers.iter().enumerate() {
            let backup_connection_string = format!("mqtt://{backup}");
            match Box::pin(
                self.client
                    .connect_with_options(&backup_connection_string, options.clone()),
            )
            .await
            {
                Ok(result) => {
                    self.session_present
                        .store(result.session_present, Ordering::Relaxed);
                    info!(
                        "Bridge '{}' connected to backup broker: {}",
                        self.config.name, backup
                    );
                    self.update_connected_stats(ConnectedBroker::Backup(idx), backup)
                        .await;
                    return Ok(ConnectedBroker::Backup(idx));
                }
                Err(e) => {
                    warn!("Failed to connect to backup broker {}: {}", backup, e);
                    self.update_error_stats(e.to_string()).await;
                }
            }
        }

        Err(BridgeError::ConnectionFailed(
            "Failed to connect to any broker".to_string(),
        ))
    }

    /// Attempts to connect using QUIC to primary and backup brokers
    async fn connect_quic(
        &self,
        options: &ConnectOptions,
        secure: bool,
    ) -> Result<ConnectedBroker> {
        let scheme = if secure { "quics" } else { "quic" };
        let connection_string = format!("{scheme}://{}", self.config.remote_address);

        if !secure || self.config.insecure == Some(true) {
            self.client.set_insecure_tls(true).await;
        }

        if let Some(strategy) = self.config.quic_stream_strategy {
            self.client.set_quic_stream_strategy(strategy).await;
        }
        if let Some(enable) = self.config.quic_flow_headers {
            self.client.set_quic_flow_headers(enable).await;
        }
        if let Some(enable) = self.config.quic_datagrams {
            self.client.set_quic_datagrams(enable).await;
        }
        if let Some(server_name) = &self.config.tls_server_name {
            let host = self
                .config
                .remote_address
                .rsplit_once(':')
                .map_or(self.config.remote_address.as_str(), |(host, _)| host);
            self.client.set_quic_server_name(host, server_name).await;
        }
        if let Some(max) = self.config.quic_max_streams {
            self.client.set_quic_max_streams(Some(max)).await;
        }

        if let Err(e) = self.configure_quic_tls().await {
            self.update_error_stats(e.to_string()).await;
            return Err(e);
        }

        match Box::pin(
            self.client
                .connect_with_options(&connection_string, options.clone()),
        )
        .await
        {
            Ok(result) => {
                self.session_present
                    .store(result.session_present, Ordering::Relaxed);
                info!(
                    "Bridge '{}' connected to primary broker: {} (QUIC{})",
                    self.config.name,
                    self.config.remote_address,
                    if secure { "S" } else { "" }
                );
                self.update_connected_stats(ConnectedBroker::Primary, &self.config.remote_address)
                    .await;
                return Ok(ConnectedBroker::Primary);
            }
            Err(e) => {
                warn!("Failed to connect to primary broker via QUIC: {e}");
                self.update_error_stats(e.to_string()).await;
            }
        }

        for (idx, backup) in self.config.backup_brokers.iter().enumerate() {
            let backup_connection_string = format!("{scheme}://{backup}");
            match Box::pin(
                self.client
                    .connect_with_options(&backup_connection_string, options.clone()),
            )
            .await
            {
                Ok(result) => {
                    self.session_present
                        .store(result.session_present, Ordering::Relaxed);
                    info!(
                        "Bridge '{}' connected to backup broker: {} (QUIC{})",
                        self.config.name,
                        backup,
                        if secure { "S" } else { "" }
                    );
                    self.update_connected_stats(ConnectedBroker::Backup(idx), backup)
                        .await;
                    return Ok(ConnectedBroker::Backup(idx));
                }
                Err(e) => {
                    warn!(
                        "Failed to connect to backup broker {} via QUIC: {}",
                        backup, e
                    );
                    self.update_error_stats(e.to_string()).await;
                }
            }
        }

        Err(BridgeError::ConnectionFailed(
            "Failed to connect to any broker via QUIC".to_string(),
        ))
    }

    /// Attempts connection with a specific protocol
    async fn connect_with_protocol(
        &self,
        protocol: BridgeProtocol,
        options: &ConnectOptions,
    ) -> Result<ConnectedBroker> {
        match protocol {
            BridgeProtocol::Tcp => {
                if self.config.use_tls {
                    Box::pin(self.connect_tls(options)).await
                } else {
                    Box::pin(self.connect_plain(options)).await
                }
            }
            BridgeProtocol::Tls => Box::pin(self.connect_tls(options)).await,
            BridgeProtocol::Quic | BridgeProtocol::QuicSecure => {
                Box::pin(self.connect_quic(options, true)).await
            }
        }
    }

    async fn try_all_protocols(
        &self,
        options: &ConnectOptions,
    ) -> Result<(ConnectedBroker, Vec<BridgeProtocol>)> {
        let max_retries = self.config.connection_retries.max(1);

        for attempt in 1..=max_retries {
            if let Ok(broker) = self
                .connect_with_protocol(self.config.protocol, options)
                .await
            {
                return Ok((broker, vec![self.config.protocol]));
            }

            if attempt < max_retries {
                let delay = apply_jitter(self.config.first_retry_delay, self.config.retry_jitter);
                debug!(
                    "Bridge '{}' {:?} attempt {}/{} failed, retrying in {:?}",
                    self.config.name, self.config.protocol, attempt, max_retries, delay
                );
                tokio::time::sleep(delay).await;
            }
        }

        let fallbacks = fallback_protocols(&self.config);
        if !fallbacks.is_empty() {
            warn!(
                "Bridge '{}' primary protocol {:?} failed after {} attempts, trying fallbacks",
                self.config.name, self.config.protocol, max_retries
            );
        }

        for (index, &fallback) in fallbacks.iter().enumerate() {
            if let Ok(broker) = self.connect_with_protocol(fallback, options).await {
                info!(
                    "Bridge '{}' connected via fallback protocol {:?}",
                    self.config.name, fallback
                );
                let tried = std::iter::once(self.config.protocol)
                    .chain(fallbacks[..=index].iter().copied())
                    .collect();
                return Ok((broker, tried));
            }
        }

        Err(BridgeError::ConnectionFailed(format!(
            "All protocols failed for bridge '{}'",
            self.config.name
        )))
    }

    async fn connect(&self) -> Result<ConnectedBroker> {
        let options = self.build_connect_options();
        let failing_back = self.failing_back.swap(false, Ordering::Relaxed);
        let mut attempt = 0u32;

        loop {
            attempt += 1;

            {
                let mut stats = self.stats.write().await;
                stats.connection_attempts += 1;
            }

            match self.try_all_protocols(&options).await {
                Ok((broker, tried)) => {
                    if matches!(broker, ConnectedBroker::Primary) {
                        self.failed_failbacks.store(0, Ordering::Relaxed);
                    } else if failing_back {
                        self.failed_failbacks.fetch_add(1, Ordering::Relaxed);
                    }
                    if matches!(broker, ConnectedBroker::Backup(_)) && self.config.enable_failback {
                        self.start_health_check(tried).await;
                    }
                    return Ok(broker);
                }
                Err(e) => {
                    if let Some(max) = self.config.max_reconnect_attempts {
                        if attempt >= max {
                            error!(
                                "Bridge '{}' exceeded max connection attempts ({})",
                                self.config.name, max
                            );
                            return Err(e);
                        }
                    }

                    let base_delay = if attempt == 1 {
                        self.config.first_retry_delay
                    } else {
                        let exponent = attempt.saturating_sub(2).min(30);
                        let delay_secs = self.config.initial_reconnect_delay.as_secs_f64()
                            * self.config.backoff_multiplier.powf(f64::from(exponent));
                        Duration::try_from_secs_f64(delay_secs)
                            .unwrap_or(self.config.max_reconnect_delay)
                            .max(self.config.initial_reconnect_delay)
                            .min(self.config.max_reconnect_delay)
                    };

                    let delay = apply_jitter(base_delay, self.config.retry_jitter);
                    warn!(
                        "Bridge '{}' connection attempt {} failed: {}, retrying in {:?}",
                        self.config.name, attempt, e, delay
                    );
                    tokio::time::sleep(delay).await;

                    if !self.running.load(Ordering::Relaxed) {
                        return Err(BridgeError::ConnectionFailed("Bridge stopped".to_string()));
                    }
                }
            }
        }
    }

    async fn start_health_check(&self, protocols: Vec<BridgeProtocol>) {
        self.stop_health_check().await;

        let config = self.config.clone();
        let running = self.running.clone();
        let failback = Arc::clone(&self.failback);
        let current_broker = self.current_broker.clone();
        let stats = self.stats.clone();

        let period = self.health_check_period();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(period);
            interval.tick().await;

            while running.load(Ordering::Relaxed) {
                interval.tick().await;

                if !running.load(Ordering::Relaxed) {
                    break;
                }

                if let Some(ConnectedBroker::Primary) = *current_broker.read().await {
                    debug!("Health check: already on primary, stopping");
                    break;
                }

                if Self::probe_broker(&config.remote_address, &config.name, &protocols).await {
                    info!(
                        "Bridge '{}': primary broker {} is available, triggering failback",
                        config.name, config.remote_address
                    );

                    {
                        let mut stats = stats.write().await;
                        stats.failback_attempts += 1;
                    }

                    failback.notify_one();
                    break;
                }
            }
        });

        *self.health_check_handle.write().await = Some(handle);
    }

    /// The wait between probes of the primary: the configured interval, doubled for each
    /// failback in a row that ended on a backup again, up to `max_reconnect_delay` but never
    /// below the configured interval.
    fn health_check_period(&self) -> Duration {
        let base = self.config.primary_health_check_interval;
        let cap = self.config.max_reconnect_delay.max(base);
        (0..self.failed_failbacks.load(Ordering::Relaxed))
            .try_fold(base, |period, _| {
                (period < cap).then(|| period.saturating_mul(2).min(cap))
            })
            .unwrap_or(cap)
    }

    async fn stop_health_check(&self) {
        if let Some(handle) = self.health_check_handle.write().await.take() {
            handle.abort();
        }
    }

    /// Whether the broker at `address` accepts a connection over one of `protocols`, the
    /// protocols the bridge tried up to and including the one that reached its backup, at
    /// each address the name resolves to. A TCP or TLS protocol is probed with a TCP
    /// connection, a QUIC one with a QUIC handshake that verifies no certificate, since it
    /// sends nothing; the failback connection that follows does.
    async fn probe_broker(address: &str, name: &str, protocols: &[BridgeProtocol]) -> bool {
        let Ok(addrs) = Self::resolve(address).await else {
            return false;
        };
        let mut probes: FuturesUnordered<_> = addrs
            .into_iter()
            .flat_map(|addr| {
                protocols
                    .iter()
                    .map(move |&protocol| Self::accepts(protocol, addr))
            })
            .collect();
        while let Some(accepted) = probes.next().await {
            if accepted {
                debug!(
                    "Bridge '{}': primary broker {} responded to probe",
                    name, address
                );
                return true;
            }
        }
        false
    }

    async fn accepts(protocol: BridgeProtocol, addr: std::net::SocketAddr) -> bool {
        match protocol {
            BridgeProtocol::Tcp | BridgeProtocol::Tls => Self::accepts_tcp(addr).await,
            #[cfg(feature = "transport-quic")]
            BridgeProtocol::Quic | BridgeProtocol::QuicSecure => Self::accepts_quic(addr).await,
            #[cfg(not(feature = "transport-quic"))]
            BridgeProtocol::Quic | BridgeProtocol::QuicSecure => false,
        }
    }

    async fn accepts_tcp(addr: std::net::SocketAddr) -> bool {
        matches!(
            tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(addr)).await,
            Ok(Ok(_))
        )
    }

    #[cfg(feature = "transport-quic")]
    async fn accepts_quic(addr: std::net::SocketAddr) -> bool {
        use crate::transport::{QuicConfig, QuicTransport};
        use crate::Transport;

        let mut transport = QuicTransport::new(
            QuicConfig::new(addr, "localhost")
                .with_verify_server_cert(false)
                .with_connect_timeout(PROBE_TIMEOUT),
        );
        let accepted = matches!(
            tokio::time::timeout(PROBE_TIMEOUT, transport.connect()).await,
            Ok(Ok(()))
        );
        let _ = transport.close().await;
        accepted
    }

    #[must_use]
    pub fn current_broker(&self) -> Arc<RwLock<Option<ConnectedBroker>>> {
        self.current_broker.clone()
    }

    pub async fn current_broker_address(&self) -> Option<String> {
        let broker = self.current_broker.read().await;
        match *broker {
            Some(ConnectedBroker::Primary) => Some(self.config.remote_address.clone()),
            Some(ConnectedBroker::Backup(idx)) => self.config.backup_brokers.get(idx).cloned(),
            None => None,
        }
    }

    /// Groups the incoming mappings by remote filter, in configuration order.
    fn ingress_filters(&self) -> Vec<IngressFilter> {
        let mut filters: Vec<IngressFilter> = Vec::new();
        for mapping in &self.config.topics {
            let core = TopicMappingCore::from(mapping);
            if !core.direction.allows_incoming() {
                continue;
            }
            let remote_filter = core.apply_remote_prefix(&core.pattern);
            match filters
                .iter_mut()
                .find(|filter| filter.remote_filter == remote_filter)
            {
                Some(filter) => {
                    if core.qos as u8 > filter.qos as u8 {
                        filter.qos = core.qos;
                    }
                    filter.mappings.push(core);
                }
                None => filters.push(IngressFilter {
                    remote_filter,
                    qos: core.qos,
                    mappings: vec![core],
                }),
            }
        }
        filters
    }

    fn ingress(&self) -> &Ingress {
        self.ingress.get_or_init(|| {
            let (qos1_tx, mut qos1_rx) = mpsc::channel::<IngressItem>(INGRESS_LANE_CAPACITY);
            let (qos0_tx, mut qos0_rx) = mpsc::channel::<Vec<PublishPacket>>(INGRESS_LANE_CAPACITY);
            let router = Arc::clone(&self.router);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        item = qos1_rx.recv() => {
                            let Some((packets, token)) = item else { break };
                            for packet in &packets {
                                router.route_message_local_only(packet, None).await;
                            }
                            token.ack();
                        }
                        item = qos0_rx.recv() => {
                            let Some(packets) = item else { break };
                            for packet in &packets {
                                router.route_message_local_only(packet, None).await;
                            }
                        }
                    }
                }
            });
            Ingress { qos1_tx, qos0_tx }
        })
    }

    /// Registers the ingress callback for every remote filter before the first connect, so a
    /// backlog the remote flushes right after CONNACK (it may still hold this bridge's
    /// session) is routed instead of being auto-acknowledged and lost.
    async fn register_ingress_callbacks(&self) -> Result<()> {
        let filters = Arc::new(self.ingress_filters());
        let ingress = self.ingress();
        for filter in filters.iter() {
            let callback = self.ingress_callback(
                Arc::clone(&filters),
                ingress.qos1_tx.clone(),
                ingress.qos0_tx.clone(),
            );
            self.client
                .register_ack_callback(filter.remote_filter.clone(), callback)
                .await?;
        }
        Ok(())
    }

    /// Subscribes the remote to the whole wanted set on every connection: to each distinct
    /// remote filter, or, when the remote does not support subscription identifiers, to the
    /// filters no other filter covers. When the remote held no session, each asks only for
    /// the retained messages of a new subscription. When it kept its session, each asks for
    /// them if a mapping it reaches has not had them from that broker, and otherwise for
    /// none. Filters the broker does not hold yet are subscribed first, so while a broker that
    /// kept the session changes how it is subscribed, a message reaches a mapping twice rather
    /// than not at all. Then it unsubscribes the filters it subscribed on that broker and no
    /// longer wants.
    async fn setup_subscriptions(&self, broker: ConnectedBroker) -> Result<()> {
        let with_identifiers = self.client.subscription_identifiers_available().await;
        let session_present = self.session_present.load(Ordering::Relaxed);
        let mut subscribed = self.subscribed.lock().await;
        let filters = Arc::new(self.ingress_filters());
        let ingress = self.ingress();
        let mut subscriptions: Vec<(String, QoS, Option<u32>)> = if with_identifiers {
            filters
                .iter()
                .enumerate()
                .map(|(index, filter)| {
                    let identifier = u32::try_from(index + 1).unwrap_or(u32::MAX);
                    (filter.remote_filter.clone(), filter.qos, Some(identifier))
                })
                .collect()
        } else {
            self.subscriptions_without_identifiers(&filters)
        };
        subscriptions.sort_by_key(|(filter, _, _)| subscribed.holds(broker, filter));

        for (remote_filter, qos, subscription_identifier) in &subscriptions {
            let reaches = reached_filters(remote_filter, &filters, with_identifiers);
            let retain_handling =
                subscribed.retain_handling(broker, remote_filter, &reaches, session_present);
            let options = SubscribeOptions {
                qos: *qos,
                subscription_identifier: *subscription_identifier,
                retain_handling,
                ..SubscribeOptions::default()
            };
            let outcome = loop {
                let callback = self.ingress_callback(
                    Arc::clone(&filters),
                    ingress.qos1_tx.clone(),
                    ingress.qos0_tx.clone(),
                );
                match self
                    .client
                    .subscribe_with_ack(remote_filter.clone(), options.clone(), callback)
                    .await
                {
                    Err(MqttError::PacketIdExhausted) if self.client.is_connected().await => {
                        tokio::time::sleep(EGRESS_RETRY_DELAY).await;
                    }
                    outcome => break outcome,
                }
            };
            if let Err(e) = outcome {
                if !self.connection_failed(&e).await {
                    return Err(e.into());
                }
                warn!(bridge = %self.config.name, filter = %remote_filter, error = %e, "connection lost while subscribing; subscribing again after reconnecting");
                self.drop_connection().await;
                return Ok(());
            }
            subscribed.subscribed(
                broker,
                remote_filter,
                &reaches,
                retain_handling,
                session_present,
            );
            info!(
                "Bridge '{}' subscribed to remote topic: {} (QoS: {:?})",
                self.config.name, remote_filter, qos
            );
        }
        let stale: Vec<String> = subscribed
            .held
            .iter()
            .filter(|(on, held)| {
                *on == broker && !subscriptions.iter().any(|(wanted, _, _)| wanted == held)
            })
            .map(|(_, held)| held.clone())
            .collect();
        for remote_filter in stale {
            let outcome = loop {
                match self.client.unsubscribe(remote_filter.clone()).await {
                    Err(MqttError::PacketIdExhausted) if self.client.is_connected().await => {
                        tokio::time::sleep(EGRESS_RETRY_DELAY).await;
                    }
                    outcome => break outcome,
                }
            };
            if let Err(e) = outcome {
                if !self.client.is_connected().await || self.connection_failed(&e).await {
                    warn!(bridge = %self.config.name, filter = %remote_filter, error = %e, "connection lost while unsubscribing; unsubscribing again after reconnecting");
                    self.drop_connection().await;
                    return Ok(());
                }
                debug!(bridge = %self.config.name, filter = %remote_filter, error = %e, "unsubscribe of a filter no longer subscribed failed");
            }
            subscribed.unsubscribed(broker, &remote_filter);
        }

        Ok(())
    }

    async fn connection_failed(&self, error: &MqttError) -> bool {
        if matches!(error, MqttError::SubscriptionDenied(_)) || self.refused_by_disconnect() {
            return false;
        }
        matches!(
            error,
            MqttError::Io(_)
                | MqttError::Timeout
                | MqttError::NotConnected
                | MqttError::OutcomeUnknown
                | MqttError::ConnectionError(_)
                | MqttError::ConnectionClosedByPeer
        ) || !self.client.is_connected().await
    }

    fn refused_by_disconnect(&self) -> bool {
        matches!(
            *self
                .server_disconnect
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            Some(
                ReasonCode::TopicFilterInvalid
                    | ReasonCode::SharedSubscriptionsNotSupported
                    | ReasonCode::SubscriptionIdentifiersNotSupported
                    | ReasonCode::WildcardSubscriptionsNotSupported
            )
        )
    }

    async fn watch_disconnects(&self) -> Result<()> {
        if self.watching_disconnects.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        let server_disconnect = Arc::clone(&self.server_disconnect);
        self.client
            .on_connection_event(move |event| {
                let reason = match event {
                    ConnectionEvent::Disconnected {
                        reason: DisconnectReason::ServerDisconnect(reason_code),
                    } => Some(reason_code),
                    _ => None,
                };
                *server_disconnect
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = reason;
            })
            .await?;
        Ok(())
    }

    async fn drop_connection(&self) {
        if let Err(e) = self.client.disconnect().await {
            debug!(bridge = %self.config.name, error = %e, "bridge client disconnect after a failed setup failed");
        }
    }

    fn subscriptions_without_identifiers(
        &self,
        filters: &[IngressFilter],
    ) -> Vec<(String, QoS, Option<u32>)> {
        let subscriptions = subscriptions_without_identifiers(filters);
        info!(
            bridge = %self.config.name,
            filters = filters.len(),
            subscriptions = subscriptions.len(),
            "Remote does not support subscription identifiers; subscribing without them, to the filters no other filter covers"
        );
        for (index, (first, _)) in subscriptions.iter().enumerate() {
            for (second, _) in &subscriptions[index + 1..] {
                if filters_overlap(first, second) {
                    warn!(
                        bridge = %self.config.name,
                        first = %first,
                        second = %second,
                        "Remote does not support subscription identifiers and these remote filters overlap; a message matching both can be delivered locally more than once, and a shared filter's mappings also get what the remote delivered through the other filter"
                    );
                }
            }
        }
        subscriptions
            .into_iter()
            .map(|(filter, qos)| (filter, qos, None))
            .collect()
    }

    /// The callback every remote filter shares: maps one remote delivery to its local
    /// copies, then hands them (with the token) to the routing task.
    fn ingress_callback(
        &self,
        filters: Arc<Vec<IngressFilter>>,
        qos1_tx: mpsc::Sender<IngressItem>,
        qos0_tx: mpsc::Sender<Vec<PublishPacket>>,
    ) -> impl Fn(PublishPacket, AckToken) + Send + Sync + 'static {
        let bridge_name = self.config.name.clone();
        let stats_received = Arc::clone(&self.messages_received);
        let stats_bytes = Arc::clone(&self.bytes_received);
        let noted_missing_ids = Arc::new(AtomicBool::new(false));

        move |mut publish, token| {
            stats_received.fetch_add(1, Ordering::Relaxed);
            stats_bytes.fetch_add(publish.payload.len() as u64, Ordering::Relaxed);

            let ids = publish.properties.subscription_identifiers();
            publish.properties.remove_subscription_identifiers();
            publish.properties.inject_sender(None);
            publish.properties.inject_client_id(None);

            let mappings: Vec<&TopicMappingCore> = if ids.is_empty() {
                if !noted_missing_ids.swap(true, Ordering::Relaxed) {
                    debug!(
                        bridge = %bridge_name,
                        "Remote sends no subscription identifiers; routing by topic"
                    );
                }
                filters
                    .iter()
                    .filter(|filter| {
                        let (remote_filter, _) = parse_shared_subscription(&filter.remote_filter);
                        topic_matches_filter(&publish.topic_name, remote_filter)
                    })
                    .flat_map(|filter| filter.mappings.iter())
                    .collect()
            } else {
                ids.iter()
                    .filter_map(|id| usize::try_from(*id).ok())
                    .filter_map(|id| id.checked_sub(1))
                    .filter_map(|index| filters.get(index))
                    .flat_map(|filter| filter.mappings.iter())
                    .collect()
            };

            let packets: Vec<PublishPacket> = mappings
                .into_iter()
                .map(|mapping| {
                    let mut packet = publish.clone();
                    packet.topic_name = mapping.apply_local_prefix(&publish.topic_name);
                    if (mapping.qos as u8) < (packet.qos as u8) {
                        packet.qos = mapping.qos;
                    }
                    packet
                })
                .collect();

            if publish.qos == QoS::AtMostOnce {
                if qos0_tx.try_send(packets).is_err() {
                    debug!(bridge = %bridge_name, "Dropping QoS 0 remote delivery: ingress lane full");
                }
                token.ack();
                return;
            }
            match qos1_tx.try_send((packets, token)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full((_, token))) => {
                    warn!(bridge = %bridge_name, "Ingress lane full; rejecting remote delivery");
                    token.reject(ReasonCode::QuotaExceeded);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    debug!(bridge = %bridge_name, "Bridge stopping; remote delivery dropped");
                }
            }
        }
    }

    /// Queues a message for the remote broker, behind those already queued.
    pub fn forward_message(&self, packet: &PublishPacket) {
        let bridge_name = &self.config.name;
        let is_running = self.running.load(Ordering::Relaxed);

        debug!(
            bridge = %bridge_name,
            topic = %packet.topic_name,
            running = is_running,
            "forward_message called"
        );

        if !is_running {
            return;
        }

        let mappings: Vec<TopicMappingCore> = self
            .config
            .topics
            .iter()
            .map(TopicMappingCore::from)
            .collect();
        let Some(decision) = evaluate_forwarding(&packet.topic_name, &mappings, true) else {
            debug!(
                bridge = %bridge_name,
                topic = %packet.topic_name,
                "no matching topic pattern found"
            );
            return;
        };

        let remote_topic = decision.transformed_topic;
        debug!(
            bridge = %bridge_name,
            local_topic = %packet.topic_name,
            remote_topic = %remote_topic,
            "topic matched, queueing for publish"
        );

        let msg_props: crate::types::MessageProperties = packet.properties.clone().into();
        let options = PublishOptions {
            qos: decision.qos,
            retain: packet.retain,
            properties: msg_props.into(),
            skip_codec: false,
        };
        if let Some(dropped) =
            self.egress()
                .push_back(remote_topic, packet.payload.clone(), options)
        {
            warn!(bridge = %bridge_name, topic = %dropped.topic, "outbound queue full, dropped the oldest message");
        }
    }

    fn egress(&self) -> &Arc<OutboundQueue> {
        self.egress.get_or_init(|| {
            let queue = Arc::new(OutboundQueue::new(self.config.outbound_queue_capacity));
            let unsettled = Arc::clone(self.unsettled.get_or_init(|| {
                Arc::new(Unsettled {
                    publishes: Mutex::new(BTreeMap::new()),
                    queue: Arc::clone(&queue),
                    bridge_name: self.config.name.clone(),
                })
            }));
            let egress = Egress {
                client: Arc::downgrade(&self.client),
                queue: Arc::clone(&queue),
                unsettled,
                messages_sent: Arc::clone(&self.messages_sent),
                bytes_sent: Arc::clone(&self.bytes_sent),
                bridge_name: self.config.name.clone(),
            };
            tokio::spawn(egress.run(self.link.subscribe()));
            queue
        })
    }

    /// Updates stats when connected
    async fn update_connected_stats(&self, broker: ConnectedBroker, address: &str) {
        let mut stats = self.stats.write().await;
        stats.connected = true;
        stats.connected_since = Some(Instant::now());
        stats.last_error = None;
        stats.current_broker = Some(address.to_string());
        stats.on_primary = matches!(broker, ConnectedBroker::Primary);

        *self.current_broker.write().await = Some(broker);
    }

    /// Updates stats when an error occurs
    async fn update_error_stats(&self, error: String) {
        let mut stats = self.stats.write().await;
        stats.connected = false;
        self.link.send_replace(false);
        stats.connected_since = None;
        stats.last_error = Some(error);
    }

    /// Gets current statistics
    pub async fn get_stats(&self) -> BridgeStats {
        let mut stats = self.stats.read().await.clone();
        stats.messages_sent = self.messages_sent.load(Ordering::Relaxed);
        stats.messages_received = self.messages_received.load(Ordering::Relaxed);
        stats.bytes_sent = self.bytes_sent.load(Ordering::Relaxed);
        stats.bytes_received = self.bytes_received.load(Ordering::Relaxed);
        stats
    }

    /// Runs the bridge connection with automatic reconnection.
    ///
    /// # Errors
    /// Returns an error if the maximum reconnect attempts is exceeded, or if setting up a
    /// connection fails, for example when the remote refuses a subscription. Outbound
    /// forwarding then stops.
    pub async fn run(&self) -> Result<()> {
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        while self.running.load(Ordering::Relaxed) {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    info!("Bridge '{}' received shutdown signal", self.config.name);
                    break;
                }
                () = self.failback.notified() => {
                    if matches!(*self.current_broker.read().await, Some(ConnectedBroker::Backup(_))) {
                        self.leave_backup().await;
                    }
                }
                result = self.run_connection() => {
                    if !self.running.load(Ordering::Relaxed) {
                        break;
                    }

                    if let Err(e) = result {
                        self.link.send_replace(false);
                        return Err(e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Ends the connection to a backup broker so that the next one tries the primary first.
    async fn leave_backup(&self) {
        info!(
            "Bridge '{}' failing back to the primary broker",
            self.config.name
        );
        self.failing_back.store(true, Ordering::Relaxed);
        if let Err(e) = self.client.disconnect().await {
            debug!(bridge = %self.config.name, error = %e, "bridge client disconnect failed during failback");
        }
        *self.current_broker.write().await = None;
        let mut stats = self.stats.write().await;
        stats.connected = false;
        self.link.send_replace(false);
        stats.connected_since = None;
    }

    async fn mark_disconnected(&self) {
        self.stop_health_check().await;
        *self.current_broker.write().await = None;
        let mut stats = self.stats.write().await;
        stats.connected = false;
        self.link.send_replace(false);
        stats.connected_since = None;
    }

    /// Runs a single connection until disconnected
    async fn run_connection(&self) -> Result<()> {
        if self.client.is_connected().await {
            self.link.send_replace(true);
        } else {
            self.client.set_queue_on_disconnect(false).await;
            self.register_ingress_callbacks().await?;
            self.watch_disconnects().await?;
            self.client.forget_ack_subscriptions().await;
            let broker = Box::pin(self.connect()).await?;
            if let Err(e) = self.setup_subscriptions(broker).await {
                self.drop_connection().await;
                self.mark_disconnected().await;
                return Err(e);
            }
            if let Some(unsettled) = self.unsettled.get() {
                unsettled.requeue_lost();
            }
            if self.client.is_connected().await {
                self.link.send_replace(true);
            }
        }

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            if !self.client.is_connected().await {
                warn!(
                    "Bridge '{}' disconnected from remote broker",
                    self.config.name
                );
                self.mark_disconnected().await;
                break;
            }

            if !self.running.load(Ordering::Relaxed) {
                break;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{filters_overlap, subscriptions_without_identifiers, IngressFilter};
    use crate::QoS;

    fn filter(remote_filter: &str, qos: QoS) -> IngressFilter {
        IngressFilter {
            remote_filter: remote_filter.to_string(),
            qos,
            mappings: Vec::new(),
        }
    }

    #[test]
    fn filters_overlap_when_some_topic_matches_both() {
        for (first, second, overlap) in [
            ("a/#", "a/b", true),
            ("a/#", "a", true),
            ("a/+/c", "a/b/#", true),
            ("a/+", "a/b/c", false),
            ("a/b", "a/c", false),
            ("+/x", "y/+", true),
            ("#", "$SYS/#", false),
            ("+/a", "$SYS/a", false),
            ("$SYS/#", "$SYS/broker", true),
            ("a/b", "a/b/c", false),
            ("$share/g/a/b", "a/#", true),
            ("$share/g/a/b", "$share/h/a/c", false),
        ] {
            assert_eq!(
                filters_overlap(first, second),
                overlap,
                "{first} and {second}"
            );
            assert_eq!(
                filters_overlap(second, first),
                overlap,
                "{second} and {first}"
            );
        }
    }

    #[test]
    fn without_identifiers_only_filters_no_other_covers_are_subscribed() {
        let filters = [
            filter("sensors/+/temp", QoS::ExactlyOnce),
            filter("sensors/#", QoS::AtMostOnce),
            filter("alerts/x", QoS::AtLeastOnce),
            filter("a/+/c", QoS::AtMostOnce),
            filter("a/b/#", QoS::AtMostOnce),
        ];
        assert_eq!(
            subscriptions_without_identifiers(&filters),
            vec![
                ("sensors/#".to_string(), QoS::ExactlyOnce),
                ("alerts/x".to_string(), QoS::AtLeastOnce),
                ("a/+/c".to_string(), QoS::AtMostOnce),
                ("a/b/#".to_string(), QoS::AtMostOnce),
            ]
        );
    }

    #[test]
    fn without_identifiers_filters_that_cover_each_other_keep_the_first() {
        let filters = [
            filter("+/#", QoS::AtMostOnce),
            filter("#", QoS::AtLeastOnce),
        ];
        assert_eq!(
            subscriptions_without_identifiers(&filters),
            vec![("+/#".to_string(), QoS::AtLeastOnce)]
        );
    }

    #[test]
    fn without_identifiers_shared_filters_are_never_dropped() {
        let filters = [
            filter("$share/g/a/b", QoS::AtLeastOnce),
            filter("$share/g/a/#", QoS::AtMostOnce),
            filter("a/#", QoS::AtMostOnce),
        ];
        assert_eq!(
            subscriptions_without_identifiers(&filters),
            vec![
                ("$share/g/a/b".to_string(), QoS::AtLeastOnce),
                ("$share/g/a/#".to_string(), QoS::AtMostOnce),
                ("a/#".to_string(), QoS::AtMostOnce),
            ]
        );
    }
}
