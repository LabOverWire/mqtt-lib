mod auth;
mod connect;
mod lifecycle;
mod publish;
mod subscribe;

use crate::broker::auth::AuthProvider;
use crate::broker::config::BrokerConfig;
use crate::broker::events::{ClientConnectEvent, ClientDisconnectEvent};
use crate::broker::resource_monitor::ResourceMonitor;
use crate::broker::router::{
    DeliveryLanes, MessageRouter, Release, RoutableMessage, TakeoverNotice, ROUTE_BUDGET_MAX,
};
use crate::broker::storage::{
    ClientSession, DynamicStorage, InflightDirection, QueueHandle, QueuedMessage, StorageBackend,
};
use crate::broker::sys_topics::BrokerStats;
use crate::broker::transport::BrokerTransport;
use crate::error::{MqttError, Result};
use crate::packet::connect::ConnectPacket;
use crate::packet::disconnect::DisconnectPacket;
use crate::packet::publish::PublishPacket;
use crate::packet::Packet;
use crate::protocol::v5::properties::Properties;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::time::Duration;
use crate::transport::packet_io::{encode_packet_to_buffer, read_packet_reusing_buffer};
use crate::transport::PacketIo;
use bytes::{Bytes, BytesMut};
use mqtt5_protocol::KeepaliveConfig;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{interval, timeout, timeout_at, Instant, Interval};
use tracing::{debug, info, warn};

/// Longest a new connection waits for the handler it displaced to hand the session over.
const HANDOFF_BOUND: Duration = Duration::from_secs(30);

/// Why the packet loop returned.
#[derive(Debug)]
pub(super) enum LoopExit {
    Closed,
    TakenOver(TakeoverNotice),
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lane {
    Qos1,
    Qos0,
}

#[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
use crate::broker::config::ServerDeliveryStrategy;
#[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
use crate::broker::server_stream_manager::ServerStreamManager;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum AuthState {
    NotStarted,
    InProgress,
    Completed,
}

pub(super) struct PendingConnect {
    pub(super) connect: ConnectPacket,
    pub(super) assigned_client_id: Option<String>,
}

pub(super) enum InflightPublish {
    Pending(PublishPacket),
    Handled,
}

#[allow(clippy::struct_excessive_bools)]
pub struct ClientHandler {
    pub(super) transport: BrokerTransport,
    pub(super) client_addr: SocketAddr,
    pub(super) config: Arc<BrokerConfig>,
    pub(super) router: Arc<MessageRouter>,
    pub(super) auth_provider: Arc<dyn AuthProvider>,
    pub(super) storage: Option<Arc<DynamicStorage>>,
    pub(super) stats: Arc<BrokerStats>,
    pub(super) resource_monitor: Arc<ResourceMonitor>,
    pub(super) shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    pub(super) client_id: Option<String>,
    pub(super) user_id: Option<String>,
    pub(super) keep_alive: Duration,
    pub(super) qos1_rx: mpsc::Receiver<RoutableMessage>,
    pub(super) qos1_tx: mpsc::Sender<RoutableMessage>,
    pub(super) qos0_rx: mpsc::Receiver<RoutableMessage>,
    pub(super) qos0_tx: mpsc::Sender<RoutableMessage>,
    pub(super) queue: Option<QueueHandle>,
    pub(super) window: u16,
    pub(super) generation: u64,
    pub(super) bound: bool,
    pub(super) released_rx: Option<oneshot::Receiver<()>>,
    pub(super) handoff_deadline: Option<Instant>,
    pub(super) handoff_waived: bool,
    pub(super) handoff_baseline: usize,
    pub(super) cutoff: Option<u64>,
    pub(super) clean_start: bool,
    pub(super) held: Vec<QueuedMessage>,
    pub(super) awaiting_pubcomp: HashSet<u16>,
    pub(super) inflight_order: VecDeque<u16>,
    pub(super) inflight_publishes: HashMap<u16, InflightPublish>,
    pub(super) session: Option<ClientSession>,
    pub(super) next_packet_id: u16,
    pub(super) normal_disconnect: bool,
    pub(super) disconnect_reason: Option<ReasonCode>,
    pub(super) request_problem_information: bool,
    pub(super) request_response_information: bool,
    pub(super) auth_method: Option<String>,
    pub(super) auth_state: AuthState,
    pub(super) pending_connect: Option<PendingConnect>,
    pub(super) topic_aliases: HashMap<u16, String>,
    pub(super) external_packet_rx: Option<mpsc::Receiver<(Packet, Option<u64>)>>,
    pub(super) pending_external_flow_id: Option<u64>,
    pub(super) client_receive_maximum: u16,
    pub(super) server_receive_maximum: u16,
    pub(super) client_max_packet_size: Option<u32>,
    pub(super) outbound_inflight: HashMap<u16, PublishPacket>,
    pub(super) protocol_version: u8,
    pub(super) write_buffer: BytesMut,
    pub(super) read_buffer: BytesMut,
    pub(super) skip_bridge_forwarding: bool,
    pub(super) pending_target_flow: Option<u64>,
    pub(super) flow_closed_rx: Option<mpsc::Receiver<u64>>,
    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    pub(super) flow_registry:
        Option<Arc<tokio::sync::Mutex<crate::session::quic_flow::FlowRegistry>>>,
    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    pub(super) quic_connection: Option<Arc<quinn::Connection>>,
    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    pub(super) server_stream_manager: Option<ServerStreamManager>,
    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    pub(super) server_delivery_strategy: ServerDeliveryStrategy,
    /// Feeds packets read off server-initiated QUIC data flows (the client's QoS>0 acks)
    /// back into this handler's packet loop.
    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    pub(super) quic_packet_tx: Option<mpsc::Sender<(Packet, Option<u64>)>>,
}

impl ClientHandler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        transport: BrokerTransport,
        client_addr: SocketAddr,
        config: Arc<BrokerConfig>,
        router: Arc<MessageRouter>,
        auth_provider: Arc<dyn AuthProvider>,
        storage: Option<Arc<DynamicStorage>>,
        stats: Arc<BrokerStats>,
        resource_monitor: Arc<ResourceMonitor>,
        shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Self {
        Self::new_with_external_packets(
            transport,
            client_addr,
            config,
            router,
            auth_provider,
            storage,
            stats,
            resource_monitor,
            shutdown_rx,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_external_packets(
        transport: BrokerTransport,
        client_addr: SocketAddr,
        config: Arc<BrokerConfig>,
        router: Arc<MessageRouter>,
        auth_provider: Arc<dyn AuthProvider>,
        storage: Option<Arc<DynamicStorage>>,
        stats: Arc<BrokerStats>,
        resource_monitor: Arc<ResourceMonitor>,
        shutdown_rx: tokio::sync::broadcast::Receiver<()>,
        external_packet_rx: Option<mpsc::Receiver<(Packet, Option<u64>)>>,
    ) -> Self {
        let (qos1_tx, qos1_rx) = mpsc::channel(config.client_channel_capacity);
        let (qos0_tx, qos0_rx) = mpsc::channel(config.client_channel_capacity);
        let server_receive_maximum = config.server_receive_maximum.unwrap_or(65535);
        let window = config.max_outbound_inflight;

        Self {
            transport,
            client_addr,
            config,
            router,
            auth_provider,
            storage,
            stats,
            resource_monitor,
            shutdown_rx,
            client_id: None,
            user_id: None,
            keep_alive: Duration::from_secs(60),
            qos1_rx,
            qos1_tx,
            qos0_rx,
            qos0_tx,
            queue: None,
            window,
            generation: 0,
            bound: false,
            released_rx: None,
            handoff_deadline: None,
            handoff_waived: false,
            handoff_baseline: 0,
            cutoff: None,
            clean_start: true,
            held: Vec::new(),
            awaiting_pubcomp: HashSet::new(),
            inflight_order: VecDeque::new(),
            inflight_publishes: HashMap::new(),
            session: None,
            next_packet_id: 1,
            normal_disconnect: false,
            disconnect_reason: None,
            request_problem_information: true,
            request_response_information: false,
            auth_method: None,
            auth_state: AuthState::NotStarted,
            pending_connect: None,
            topic_aliases: HashMap::new(),
            external_packet_rx,
            pending_external_flow_id: None,
            client_receive_maximum: 65535,
            server_receive_maximum,
            client_max_packet_size: None,
            outbound_inflight: HashMap::new(),
            protocol_version: 5,
            write_buffer: BytesMut::with_capacity(4096),
            read_buffer: BytesMut::with_capacity(4096),
            skip_bridge_forwarding: false,
            pending_target_flow: None,
            flow_closed_rx: None,
            #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
            flow_registry: None,
            #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
            quic_connection: None,
            #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
            server_stream_manager: None,
            #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
            server_delivery_strategy: ServerDeliveryStrategy::default(),
            #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
            quic_packet_tx: None,
        }
    }

    #[must_use]
    pub fn with_skip_bridge_forwarding(mut self, skip: bool) -> Self {
        self.skip_bridge_forwarding = skip;
        self
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    #[must_use]
    pub fn with_quic_connection(mut self, conn: Arc<quinn::Connection>) -> Self {
        self.quic_connection = Some(conn);
        self
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    #[must_use]
    pub fn with_server_delivery_strategy(mut self, strategy: ServerDeliveryStrategy) -> Self {
        self.server_delivery_strategy = strategy;
        self
    }

    /// Supplies the channel that server data flows use to return the client's QoS>0 acks.
    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    #[must_use]
    pub fn with_quic_packet_tx(mut self, tx: mpsc::Sender<(Packet, Option<u64>)>) -> Self {
        self.quic_packet_tx = Some(tx);
        self
    }

    #[must_use]
    pub fn with_flow_closed_rx(mut self, rx: mpsc::Receiver<u64>) -> Self {
        self.flow_closed_rx = Some(rx);
        self
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    #[must_use]
    pub fn with_flow_registry(
        mut self,
        registry: Arc<tokio::sync::Mutex<crate::session::quic_flow::FlowRegistry>>,
    ) -> Self {
        self.flow_registry = Some(registry);
        self
    }

    pub(super) async fn route_publish(&self, publish: &PublishPacket, client_id: Option<&str>) {
        if self.skip_bridge_forwarding {
            self.router
                .route_message_local_only(publish, client_id)
                .await;
        } else {
            let budget = if self.keep_alive.is_zero() {
                ROUTE_BUDGET_MAX
            } else {
                (self.keep_alive / 2).min(ROUTE_BUDGET_MAX)
            };
            self.router
                .route_message_with_deadline(publish, client_id, Instant::now() + budget)
                .await;
        }
    }

    /// Runs the client handler until disconnection or error
    ///
    /// # Errors
    ///
    /// Returns an error if transport operations fail or authentication fails
    ///
    /// # Panics
    ///
    /// Panics if `client_id` is None after successful connection
    pub async fn run(mut self) -> Result<()> {
        let client_id = self.perform_connect_handshake().await?;

        let (disconnect_tx, mut disconnect_rx) = oneshot::channel();

        let queue = self.router.queue_handle(&client_id);
        self.queue = Some(Arc::clone(&queue));
        self.window = self
            .client_receive_maximum
            .min(self.config.max_outbound_inflight)
            .max(1);
        let registration = self
            .router
            .register_session(
                client_id.clone(),
                DeliveryLanes {
                    qos1_tx: self.qos1_tx.clone(),
                    qos0_tx: self.qos0_tx.clone(),
                },
                Arc::clone(&queue),
                disconnect_tx,
                self.clean_start,
            )
            .await;
        self.generation = registration.generation;
        self.cutoff = registration.cutoff;
        self.handoff_deadline = registration
            .released
            .as_ref()
            .map(|_| Instant::now() + HANDOFF_BOUND);
        self.released_rx = registration.released;

        self.fire_connect_event(&client_id).await;

        let (result, exit) = match self.serve(&mut disconnect_rx).await {
            Ok(exit) => (Ok(()), exit),
            Err(e) => (Err(e), LoopExit::Closed),
        };

        let taken_over = if let LoopExit::TakenOver(notice) = exit {
            self.hand_off(&queue, notice).await;
            self.release_router_entry(&client_id).await;
            true
        } else {
            // Move this connection's unfinished deliveries back to (or off) the queue BEFORE
            // releasing the router entry, so a reconnect that races the release still sees this
            // entry, is handed a notice, and waits for the hand-off instead of binding onto
            // half-torn-down state and re-delivering.
            if self.session_preserved() {
                self.requeue_unsent(&queue).await;
            } else {
                self.drop_unsent().await;
            }
            match self.release_router_entry(&client_id).await {
                Release::Owned => {
                    queue.finish_drain();
                    if self.session_preserved() {
                        queue.notify();
                    } else {
                        queue.clear(None);
                    }
                    false
                }
                Release::Displaced => {
                    // A successor registered during the requeue above. Complete its hand-off
                    // protocol: this connection's messages are already back on the queue, so
                    // just release the successor and let its guard balance the count.
                    if let Ok(notice) = disconnect_rx.try_recv() {
                        queue.finish_drain();
                        let TakeoverNotice {
                            released, guard, ..
                        } = notice;
                        drop(guard);
                        let _ = released.send(());
                        queue.notify();
                    } else {
                        queue.finish_drain();
                    }
                    true
                }
            }
        };

        self.handle_disconnect_cleanup(&client_id, taken_over).await;

        info!("Client {} disconnected", client_id);

        result
    }

    async fn serve(
        &mut self,
        disconnect_rx: &mut oneshot::Receiver<TakeoverNotice>,
    ) -> Result<LoopExit> {
        if self.released_rx.is_none() {
            self.bind().await?;
        }
        if self.keep_alive.is_zero() {
            self.handle_packets(None, disconnect_rx).await
        } else {
            let mut keep_alive_interval = interval(self.keep_alive);
            keep_alive_interval.reset();
            self.handle_packets(Some(&mut keep_alive_interval), disconnect_rx)
                .await
        }
    }

    /// Takes ownership of the session's delivery state once no older handler can still be
    /// touching it: a clean start discards what the old session left, a resumed session
    /// reloads its persisted inflight messages at the front of the queue.
    async fn bind(&mut self) -> Result<()> {
        self.bound = true;
        self.released_rx = None;
        self.handoff_deadline = None;
        let Some(queue) = self.queue.clone() else {
            return Ok(());
        };
        self.handoff_baseline = queue.handoffs();
        if self.clean_start {
            let discarded = queue.clear(self.cutoff);
            if discarded > 0 {
                debug!(count = discarded, "Clean start discarded queued messages");
            }
            if let (Some(storage), Some(client_id)) = (&self.storage, &self.client_id) {
                if let Err(e) = storage.remove_all_inflight_messages(client_id).await {
                    debug!("failed to clear inflight messages on clean start: {e}");
                }
            }
        } else {
            self.load_persisted_inflight(&queue).await?;
        }
        queue.notify();
        Ok(())
    }

    /// The displaced handler's side of a takeover: give the session queue everything this
    /// connection still owned (or drop it on a clean start), then let the new handler go.
    async fn hand_off(&mut self, queue: &QueueHandle, notice: TakeoverNotice) {
        let TakeoverNotice {
            discard,
            released,
            guard,
        } = notice;
        if discard {
            self.drop_unsent().await;
        } else {
            self.requeue_unsent(queue).await;
        }
        queue.finish_drain();
        // Dropping the guard decrements the hand-off count and, when it reaches zero, wakes
        // the successor waiting to bind — do it before releasing so the successor sees the
        // session quiesced.
        drop(guard);
        if released.send(()).is_err() {
            debug!("New session handler went away before the hand-off completed");
        }
        queue.notify();
    }

    async fn perform_connect_handshake(&mut self) -> Result<String> {
        tracing::debug!(
            "Client handler started for {} ({})",
            self.client_addr,
            self.transport.transport_type()
        );
        let connect_timeout = Duration::from_secs(10);
        tracing::trace!(
            "Waiting for CONNECT packet with {}s timeout",
            connect_timeout.as_secs()
        );
        match timeout(connect_timeout, self.wait_for_connect()).await {
            Ok(Ok(())) => {
                let client_id = self.client_id.as_ref().unwrap().clone();
                info!(
                    "Client {} connected from {} ({})",
                    client_id,
                    self.client_addr,
                    self.transport.transport_type()
                );
                if let Some(cert_info) = self.transport.client_cert_info() {
                    debug!("Client certificate: {}", cert_info);
                }

                self.resource_monitor
                    .register_connection(client_id.clone(), self.client_addr.ip())
                    .await;

                self.stats.client_connected();
                Ok(client_id)
            }
            Ok(Err(e)) => {
                if e.to_string().contains("Connection closed") {
                    info!("Client disconnected during connect phase: {e}");
                    tracing::debug!("Connection closed error details: {:?}", e);
                } else {
                    warn!("Connect error: {e}");
                    tracing::debug!("Connect error details: {:?}", e);
                }
                Err(e)
            }
            Err(_) => {
                warn!("Connect timeout from {}", self.client_addr);
                Err(MqttError::Timeout)
            }
        }
    }

    async fn fire_connect_event(&self, client_id: &str) {
        if let Some(ref handler) = self.config.event_handler {
            let event = ClientConnectEvent {
                client_id: client_id.to_string().into(),
                user_id: self.user_id.as_deref().map(Arc::from),
                clean_start: self.clean_start,
                session_expiry_interval: self
                    .session
                    .as_ref()
                    .and_then(|s| s.expiry_interval)
                    .unwrap_or(0),
                will_topic: self
                    .session
                    .as_ref()
                    .and_then(|s| s.will_message.as_ref().map(|w| w.topic.clone().into())),
                will_payload: self.session.as_ref().and_then(|s| {
                    s.will_message
                        .as_ref()
                        .map(|w| Bytes::from(w.payload.clone()))
                }),
                will_qos: self
                    .session
                    .as_ref()
                    .and_then(|s| s.will_message.as_ref().map(|w| w.qos)),
                will_retain: self
                    .session
                    .as_ref()
                    .and_then(|s| s.will_message.as_ref().map(|w| w.retain)),
            };
            handler.on_client_connect(event).await;
        }
    }

    async fn handle_disconnect_cleanup(&mut self, client_id: &str, session_taken_over: bool) {
        #[cfg(feature = "opentelemetry")]
        {
            use tracing::Instrument;
            let span = tracing::info_span!(
                "mqtt.disconnect",
                mqtt.client_id = %client_id,
            );
            self.handle_disconnect_cleanup_inner(client_id, session_taken_over)
                .instrument(span)
                .await;
        }
        #[cfg(not(feature = "opentelemetry"))]
        self.handle_disconnect_cleanup_inner(client_id, session_taken_over)
            .await;
    }

    async fn handle_disconnect_cleanup_inner(&mut self, client_id: &str, session_taken_over: bool) {
        self.resource_monitor
            .unregister_connection(client_id, self.client_addr.ip())
            .await;

        if !session_taken_over {
            self.cleanup_session_storage(client_id).await;
        }

        if let Some(ref user_id) = self.user_id {
            self.auth_provider.cleanup_session(user_id).await;
        }

        if !self.normal_disconnect {
            #[cfg(feature = "opentelemetry")]
            {
                use tracing::Instrument;
                if let Some(ref session) = self.session {
                    if let Some(ref will) = session.will_message {
                        let span = tracing::info_span!(
                            "mqtt.will",
                            mqtt.client_id = %client_id,
                            mqtt.topic = %will.topic,
                        );
                        self.publish_will_message(client_id).instrument(span).await;
                    } else {
                        self.publish_will_message(client_id).await;
                    }
                } else {
                    self.publish_will_message(client_id).await;
                }
            }
            #[cfg(not(feature = "opentelemetry"))]
            self.publish_will_message(client_id).await;
        }

        self.fire_disconnect_event(client_id).await;
    }

    async fn release_router_entry(&self, client_id: &str) -> Release {
        let release = self
            .router
            .release_client(client_id, self.generation, self.session_preserved())
            .await;
        match release {
            Release::Owned => info!("Unregistered client {} from the router", client_id),
            Release::Displaced => info!(
                "Client {} router entry now belongs to the connection that took it over",
                client_id
            ),
        }
        release
    }

    async fn cleanup_session_storage(&self, client_id: &str) {
        if let Some(ref storage) = self.storage {
            if let Some(ref session) = self.session {
                match storage.get_session(client_id).await {
                    Ok(Some(mut stored_session)) => {
                        stored_session.touch();
                        if let Err(e) = storage.store_session(stored_session).await {
                            warn!("Failed to store session for {client_id}: {e}");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!("Failed to get session for {client_id}: {e}");
                    }
                }

                if session.expiry_interval == Some(0) {
                    if let Err(e) = storage.remove_session(client_id).await {
                        warn!("Failed to remove session for {client_id}: {e}");
                    }
                    storage.queue_handle(client_id).clear(None);
                    if let Err(e) = storage.remove_all_inflight_messages(client_id).await {
                        warn!("Failed to remove inflight messages for {client_id}: {e}");
                    }
                    debug!(
                        "Removed session, queued, and inflight messages for client {}",
                        client_id
                    );
                }
            }
        }
    }

    async fn fire_disconnect_event(&self, client_id: &str) {
        if let Some(ref handler) = self.config.event_handler {
            let event = ClientDisconnectEvent {
                client_id: client_id.to_string().into(),
                user_id: self.user_id.as_deref().map(Arc::from),
                reason: self.disconnect_reason.unwrap_or(if self.normal_disconnect {
                    ReasonCode::Success
                } else {
                    ReasonCode::UnspecifiedError
                }),
                unexpected: !self.normal_disconnect,
            };
            handler.on_client_disconnect(event).await;
        }
    }

    fn max_packet_size(&self) -> usize {
        self.config.max_packet_size
    }

    async fn write_to_client(&mut self, mut packet: Packet) -> Result<()> {
        if let Some(max) = self.client_max_packet_size {
            let max = max as usize;
            self.write_buffer.clear();
            encode_packet_to_buffer(&packet, &mut self.write_buffer)?;
            if self.write_buffer.len() > max {
                if let Some(properties) = Self::packet_properties_mut(&mut packet) {
                    properties.remove_reason_string();
                    self.write_buffer.clear();
                    encode_packet_to_buffer(&packet, &mut self.write_buffer)?;
                }
                if self.write_buffer.len() > max {
                    debug!(
                        packet = packet.packet_type_name(),
                        packet_size = self.write_buffer.len(),
                        max_packet_size = max,
                        "Discarding outbound packet exceeding client Maximum Packet Size"
                    );
                    return Ok(());
                }
            }
        }
        let write_timeout = self.write_timeout();
        let packet_name = packet.packet_type_name();
        timeout(write_timeout, self.transport.write_packet(packet))
            .await
            .map_err(|_| {
                warn!(
                    packet = packet_name,
                    timeout = ?write_timeout,
                    "Transport write stalled; treating the client as gone"
                );
                MqttError::Timeout
            })?
    }

    fn packet_properties_mut(packet: &mut Packet) -> Option<&mut Properties> {
        match packet {
            Packet::ConnAck(p) => Some(&mut p.properties),
            Packet::PubAck(p) => Some(&mut p.properties),
            Packet::PubRec(p) => Some(&mut p.properties),
            Packet::PubRel(p) => Some(&mut p.properties),
            Packet::PubComp(p) => Some(&mut p.properties),
            Packet::SubAck(p) => Some(&mut p.properties),
            Packet::UnsubAck(p) => Some(&mut p.properties),
            Packet::Auth(p) => Some(&mut p.properties),
            Packet::Disconnect(p) => Some(&mut p.properties),
            _ => None,
        }
    }

    async fn wait_for_connect(&mut self) -> Result<()> {
        let max_size = self.max_packet_size();
        let packet =
            read_packet_reusing_buffer(&mut self.transport, 5, &mut self.read_buffer, max_size)
                .await?;

        match packet {
            Packet::Connect(connect) => {
                #[cfg(feature = "opentelemetry")]
                {
                    use tracing::Instrument;
                    let span = tracing::info_span!(
                        "mqtt.connect",
                        mqtt.client_id = %connect.client_id,
                        mqtt.clean_start = connect.clean_start,
                        mqtt.protocol_version = connect.protocol_version,
                    );
                    self.handle_connect(*connect).instrument(span).await
                }
                #[cfg(not(feature = "opentelemetry"))]
                self.handle_connect(*connect).await
            }
            _ => Err(MqttError::ProtocolError(
                "Expected CONNECT packet".to_string(),
            )),
        }
    }

    /// Time one delivery batch may occupy the task before the socket is serviced again.
    pub(super) fn batch_time_budget(&self) -> Duration {
        if self.keep_alive.is_zero() {
            ROUTE_BUDGET_MAX
        } else {
            self.keep_alive / 4
        }
    }

    /// Zombie guard for one transport write; never long enough to push a live client past
    /// its keep-alive limit on its own.
    pub(super) fn write_timeout(&self) -> Duration {
        if self.keep_alive.is_zero() {
            Duration::from_secs(300)
        } else {
            KeepaliveConfig::default()
                .timeout_duration(self.keep_alive)
                .saturating_sub(self.batch_time_budget())
                .max(Duration::from_secs(1))
        }
    }

    fn session_preserved(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.expiry_interval != Some(0))
    }

    /// True while any hand-off is in progress on this client id (this handler's own, or an
    /// older handler still finishing). Gates whether a not-yet-waived handler delivers.
    pub(super) fn handing_off(&self) -> bool {
        self.queue.as_ref().is_some_and(|queue| queue.handoff())
    }

    /// True only when a NEWER connection has displaced THIS handler — i.e. the hand-off count
    /// has risen above what it was when this handler bound. A handler that merely waited out an
    /// older predecessor (equal count) is not displaced and must keep delivering, not stash.
    pub(super) fn being_displaced(&self) -> bool {
        self.queue
            .as_ref()
            .is_some_and(|queue| queue.handoffs() > self.handoff_baseline)
    }

    /// Discards everything this connection still owned: a clean-start takeover starts empty.
    async fn drop_unsent(&mut self) {
        self.qos1_rx.close();
        self.qos0_rx.close();
        while self.qos0_rx.try_recv().is_ok() {}
        while self.qos1_rx.recv().await.is_some() {}
        self.inflight_order.clear();
        self.held.clear();
        self.awaiting_pubcomp.clear();
        let unacked: Vec<u16> = self.outbound_inflight.drain().map(|(id, _)| id).collect();
        if let (Some(storage), Some(client_id)) = (self.storage.clone(), self.client_id.clone()) {
            for packet_id in unacked {
                if let Err(e) = storage
                    .remove_inflight_message(&client_id, packet_id, InflightDirection::Outbound)
                    .await
                {
                    debug!("failed to remove discarded inflight {packet_id}: {e}");
                }
            }
        }
    }

    /// Moves everything this handler still owns for the session — unacked QoS>0 messages in
    /// first-send order, then the undelivered QoS>0 lane contents — to the front of the
    /// session's queue, so the next connection (or the one that took over) resumes in order.
    async fn requeue_unsent(&mut self, queue: &QueueHandle) {
        let Some(client_id) = self.client_id.clone() else {
            return;
        };
        self.qos1_rx.close();
        self.qos0_rx.close();
        while self.qos0_rx.try_recv().is_ok() {}

        let mut unsent = Vec::new();
        let mut unacked_ids = Vec::new();
        for packet_id in std::mem::take(&mut self.inflight_order) {
            if self.awaiting_pubcomp.contains(&packet_id) {
                continue;
            }
            if let Some(publish) = self.outbound_inflight.remove(&packet_id) {
                let qos = publish.qos;
                let mut queued =
                    QueuedMessage::new(publish, client_id.clone(), qos, Some(packet_id));
                queued.dup = true;
                unsent.push(queued);
                unacked_ids.push(packet_id);
            }
        }
        self.outbound_inflight.clear();
        self.awaiting_pubcomp.clear();
        unsent.append(&mut self.held);
        while let Some(routable) = self.qos1_rx.recv().await {
            let qos = routable.publish.qos;
            unsent.push(
                QueuedMessage::new(routable.publish, client_id.clone(), qos, None)
                    .with_target_flow(routable.target_flow),
            );
        }
        if unsent.is_empty() {
            return;
        }
        debug!(
            client_id = %client_id,
            count = unsent.len(),
            "Re-queued undelivered messages for the session"
        );
        queue.requeue_front(unsent);
        if let Some(storage) = self.storage.clone() {
            for packet_id in unacked_ids {
                if let Err(e) = storage
                    .remove_inflight_message(&client_id, packet_id, InflightDirection::Outbound)
                    .await
                {
                    debug!("failed to remove re-queued inflight {packet_id}: {e}");
                }
            }
        }
    }

    /// Waits until this connection may take over the session: the immediate predecessor has
    /// released (or the deadline passed), then the whole hand-off chain has drained to zero.
    /// Returns `true` if it quiesced, `false` if the deadline forced a waive. Never resolves
    /// once bound (`deadline` is cleared in `bind`), so the select arm goes quiet afterward.
    async fn await_handoff_quiesce(
        released_rx: &mut Option<oneshot::Receiver<()>>,
        queue: Option<&QueueHandle>,
        deadline: Option<Instant>,
    ) -> bool {
        let Some(deadline) = deadline else {
            return std::future::pending::<bool>().await;
        };
        if let Some(rx) = released_rx.as_mut() {
            let _ = timeout_at(deadline, rx).await;
            *released_rx = None;
        }
        let Some(queue) = queue else {
            return true;
        };
        while queue.handoffs() != 0 {
            if timeout_at(deadline, queue.handoff_quiesced())
                .await
                .is_err()
            {
                return false;
            }
        }
        true
    }

    async fn handle_packets(
        &mut self,
        mut keep_alive_interval: Option<&mut Interval>,
        disconnect_rx: &mut oneshot::Receiver<TakeoverNotice>,
    ) -> Result<LoopExit> {
        let mut last_packet_time = Instant::now();
        let max_size = self.max_packet_size();
        let read_timeout = self.read_timeout();
        let queue = self.queue.clone();

        loop {
            let delivering = self.bound && (self.handoff_waived || !self.handing_off());
            let window_open = delivering && self.outbound_inflight.len() < usize::from(self.window);
            tokio::select! {
                quiesced = Self::await_handoff_quiesce(
                    &mut self.released_rx,
                    self.queue.as_ref(),
                    self.handoff_deadline,
                ) => {
                    if quiesced {
                        debug!("Hand-off chain drained; binding the session");
                    } else {
                        warn!("Hand-off did not drain in time; proceeding without it");
                        self.handoff_waived = true;
                    }
                    self.bind().await?;
                }

                read_result = timeout(read_timeout, read_packet_reusing_buffer(&mut self.transport, self.protocol_version, &mut self.read_buffer, max_size)) => {
                    if let Some(exit) = self.handle_read(read_result, read_timeout).await? {
                        return Ok(exit);
                    }
                    last_packet_time = Instant::now();
                }

                external_packet = async {
                    if let Some(ref mut rx) = self.external_packet_rx {
                        rx.recv().await
                    } else {
                        std::future::pending::<Option<(Packet, Option<u64>)>>().await
                    }
                } => {
                    if self.handle_external_packet(external_packet).await? {
                        last_packet_time = Instant::now();
                    }
                }

                closed_flow_id = async {
                    if let Some(ref mut rx) = self.flow_closed_rx {
                        rx.recv().await
                    } else {
                        std::future::pending::<Option<u64>>().await
                    }
                } => {
                    if let Some(flow_id) = closed_flow_id {
                        self.handle_flow_closed(flow_id).await;
                    }
                }

                routable = async {
                    if window_open {
                        self.qos1_rx.recv().await
                    } else {
                        std::future::pending::<Option<RoutableMessage>>().await
                    }
                } => {
                    if let Some(exit) = self.deliver_from_lane(routable, Lane::Qos1).await? {
                        return Ok(exit);
                    }
                }

                routable = self.qos0_rx.recv() => {
                    if let Some(exit) = self.deliver_from_lane(routable, Lane::Qos0).await? {
                        return Ok(exit);
                    }
                }

                () = async {
                    match &queue {
                        Some(queue) if delivering => queue.notified().await,
                        _ => std::future::pending::<()>().await,
                    }
                } => {
                    self.drain_backlog().await?;
                }

                () = async {
                    match keep_alive_interval.as_deref_mut() {
                        Some(interval) => {
                            interval.tick().await;
                        }
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    self.check_keep_alive(last_packet_time)?;
                }

                notice = &mut *disconnect_rx => {
                    let Ok(notice) = notice else {
                        debug!("Router dropped this connection's entry");
                        return Ok(LoopExit::Closed);
                    };
                    self.send_taken_over().await;
                    return Ok(LoopExit::TakenOver(notice));
                }

                _ = self.shutdown_rx.recv() => {
                    self.send_shutting_down().await;
                    return Ok(LoopExit::Shutdown);
                }
            }
        }
    }

    fn read_timeout(&self) -> Duration {
        if self.keep_alive.is_zero() {
            Duration::from_secs(300)
        } else {
            self.keep_alive.mul_f32(1.5)
        }
    }

    async fn handle_external_packet(
        &mut self,
        external_packet: Option<(Packet, Option<u64>)>,
    ) -> Result<bool> {
        let Some((packet, flow_id)) = external_packet else {
            return Ok(false);
        };
        self.pending_external_flow_id = flow_id;
        let result = self.handle_packet(packet).await;
        self.pending_external_flow_id = None;
        result.map(|()| true)
    }

    async fn deliver_from_lane(
        &mut self,
        routable: Option<RoutableMessage>,
        lane: Lane,
    ) -> Result<Option<LoopExit>> {
        let Some(routable) = routable else {
            warn!(?lane, "Delivery lane closed unexpectedly");
            return Ok(Some(LoopExit::Closed));
        };
        self.send_lane_batch(routable, lane).await?;
        Ok(None)
    }

    fn check_keep_alive(&self, last_packet_time: Instant) -> Result<()> {
        let timeout_duration = KeepaliveConfig::default().timeout_duration(self.keep_alive);
        if last_packet_time.elapsed() > timeout_duration {
            warn!("Keep-alive timeout");
            return Err(MqttError::KeepAliveTimeout);
        }
        Ok(())
    }

    async fn handle_read(
        &mut self,
        read_result: std::result::Result<Result<Packet>, tokio::time::error::Elapsed>,
        read_timeout: Duration,
    ) -> Result<Option<LoopExit>> {
        let packet_result = read_result.map_err(|_| {
            warn!("Read timeout (no data for {:?})", read_timeout);
            MqttError::Timeout
        })?;
        match packet_result {
            Ok(packet) => {
                self.handle_packet(packet).await?;
                #[cfg(not(target_arch = "wasm32"))]
                self.check_quic_migration().await;
                Ok(None)
            }
            Err(e) if e.is_normal_disconnect() => {
                debug!("Client disconnected");
                Ok(Some(LoopExit::Closed))
            }
            Err(e) => Err(e),
        }
    }

    async fn send_taken_over(&mut self) {
        info!("Session taken over by another client");
        let disconnect = Packet::Disconnect(DisconnectPacket {
            reason_code: ReasonCode::SessionTakenOver,
            properties: Properties::default(),
        });
        if let Err(e) = self.write_to_client(disconnect).await {
            debug!("Failed to send session-taken-over DISCONNECT: {e}");
        }
    }

    async fn send_shutting_down(&mut self) {
        debug!("Shutdown signal received");
        if self.protocol_version == 5 {
            let disconnect = DisconnectPacket::new(ReasonCode::ServerShuttingDown);
            if let Err(e) = self.write_to_client(Packet::Disconnect(disconnect)).await {
                debug!("Failed to send server-shutting-down DISCONNECT: {e}");
            }
        }
    }

    /// Sends `first` and up to `drain_batch - 1` more messages already waiting on the lane,
    /// stopping when the window closes (QoS>0 lane) or the time budget is spent, so the
    /// socket is serviced between batches.
    async fn send_lane_batch(&mut self, first: RoutableMessage, lane: Lane) -> Result<()> {
        let started = Instant::now();
        let budget = self.batch_time_budget();
        let batch = self.config.drain_batch.max(1);
        self.send_routable(first).await?;
        for _ in 1..batch {
            if started.elapsed() > budget || self.being_displaced() {
                break;
            }
            let next = match lane {
                Lane::Qos1 => {
                    if self.outbound_inflight.len() >= usize::from(self.window) {
                        break;
                    }
                    self.qos1_rx.try_recv()
                }
                Lane::Qos0 => self.qos0_rx.try_recv(),
            };
            match next {
                Ok(routable) => self.send_routable(routable).await?,
                Err(_) => break,
            }
        }
        if lane == Lane::Qos1 && self.qos1_rx.is_empty() {
            if let Some(queue) = &self.queue {
                if queue.count() > 0 {
                    queue.notify();
                }
            }
        }
        Ok(())
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    async fn check_quic_migration(&mut self) {
        let Some(conn) = &self.quic_connection else {
            return;
        };
        let current_addr = conn.remote_address();
        if current_addr != self.client_addr {
            let old_addr = self.client_addr;
            self.client_addr = current_addr;
            if let Some(ref client_id) = self.client_id {
                info!(
                    client_id = %client_id,
                    old_addr = %old_addr,
                    new_addr = %current_addr,
                    "QUIC connection migrated"
                );
                self.resource_monitor
                    .update_connection_ip(client_id, old_addr.ip(), current_addr.ip())
                    .await;
            }
        }
    }

    #[cfg(any(target_arch = "wasm32", not(feature = "transport-quic")))]
    fn check_quic_migration(&mut self) -> impl std::future::Future<Output = ()> {
        let _ = &self;
        std::future::ready(())
    }

    async fn handle_packet(&mut self, packet: Packet) -> Result<()> {
        #[cfg(feature = "opentelemetry")]
        return self.handle_packet_instrumented(packet).await;
        #[cfg(not(feature = "opentelemetry"))]
        self.handle_packet_inner(packet).await
    }

    #[cfg(feature = "opentelemetry")]
    async fn handle_packet_instrumented(&mut self, packet: Packet) -> Result<()> {
        use tracing::Instrument;

        match packet {
            Packet::Publish(publish) => {
                let span = tracing::info_span!(
                    "mqtt.publish.receive",
                    mqtt.client_id = self.client_id.as_deref().unwrap_or(""),
                    mqtt.topic = %publish.topic_name,
                    mqtt.qos = publish.qos as u8,
                    mqtt.retain = publish.retain,
                    mqtt.payload_size = publish.payload.len(),
                );
                self.handle_publish(publish).instrument(span).await
            }
            Packet::Subscribe(subscribe) => {
                let span = tracing::info_span!(
                    "mqtt.subscribe",
                    mqtt.client_id = self.client_id.as_deref().unwrap_or(""),
                    mqtt.filter_count = subscribe.filters.len(),
                );
                self.handle_subscribe(subscribe).instrument(span).await
            }
            Packet::Unsubscribe(unsubscribe) => {
                let span = tracing::info_span!(
                    "mqtt.unsubscribe",
                    mqtt.client_id = self.client_id.as_deref().unwrap_or(""),
                    mqtt.filter_count = unsubscribe.filters.len(),
                );
                self.handle_unsubscribe(unsubscribe).instrument(span).await
            }
            Packet::PubAck(ref puback) => {
                let span = tracing::info_span!("mqtt.puback", mqtt.packet_id = puback.packet_id);
                self.handle_puback(puback).instrument(span).await;
                Ok(())
            }
            Packet::PubRec(pubrec) => {
                let span = tracing::info_span!("mqtt.pubrec", mqtt.packet_id = pubrec.packet_id);
                self.handle_pubrec(pubrec).instrument(span).await
            }
            Packet::PubRel(pubrel) => {
                let span = tracing::info_span!("mqtt.pubrel", mqtt.packet_id = pubrel.packet_id);
                self.handle_pubrel(pubrel).instrument(span).await
            }
            Packet::PubComp(ref pubcomp) => {
                let span = tracing::info_span!("mqtt.pubcomp", mqtt.packet_id = pubcomp.packet_id);
                self.handle_pubcomp(pubcomp).instrument(span).await;
                Ok(())
            }
            other => self.handle_packet_inner(other).await,
        }
    }

    async fn handle_packet_inner(&mut self, packet: Packet) -> Result<()> {
        match packet {
            Packet::Connect(_) => {
                if self.protocol_version == 5 {
                    let disconnect = DisconnectPacket::new(ReasonCode::ProtocolError);
                    self.write_to_client(Packet::Disconnect(disconnect)).await?;
                }
                Err(MqttError::ProtocolError("Duplicate CONNECT".to_string()))
            }
            Packet::Subscribe(subscribe) => self.handle_subscribe(subscribe).await,
            Packet::Unsubscribe(unsubscribe) => self.handle_unsubscribe(unsubscribe).await,
            Packet::Publish(publish) => self.handle_publish(publish).await,
            Packet::PubAck(ref puback) => {
                self.handle_puback(puback).await;
                Ok(())
            }
            Packet::PubRec(pubrec) => self.handle_pubrec(pubrec).await,
            Packet::PubRel(pubrel) => self.handle_pubrel(pubrel).await,
            Packet::PubComp(ref pubcomp) => {
                self.handle_pubcomp(pubcomp).await;
                Ok(())
            }
            Packet::PingReq => self.handle_pingreq().await,
            Packet::Disconnect(disconnect) => self.handle_disconnect(&disconnect),
            Packet::Auth(auth) => self.handle_auth(auth).await,
            _ => {
                warn!("Unexpected packet type");
                Ok(())
            }
        }
    }
}

impl Drop for ClientHandler {
    fn drop(&mut self) {
        if let Some(ref client_id) = self.client_id {
            debug!("Client handler dropped for {}", client_id);
            self.stats.client_disconnected();
        }
    }
}
