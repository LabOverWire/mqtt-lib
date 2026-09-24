//! Direct async client implementation
//!
//! This module implements the MQTT client using direct async calls.

pub(crate) mod ack;
mod handlers;
mod keepalive;
mod outbound;
mod reader;
mod replay;
mod tracking;
mod unified;

pub use ack::AckToken;
pub(crate) use ack::{AckCallbackManager, AckDispatcher};

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Duration;

use crate::callback::{CallbackId, CallbackManager};
use crate::client::auth_handler::{AuthHandler, AuthResponse};
use crate::client::publish_outcome::{
    Delivery, IndeterminateReason, PublishHandle, PublishOutcome, PublishRejection, PublishResult,
};
use crate::error::{MqttError, Result};
use crate::packet::auth::AuthPacket;
use crate::packet::connect::ConnectPacket;
use crate::packet::publish::PublishPacket;
use crate::packet::suback::{SubAckPacket, SubAckReasonCode};
use crate::packet::subscribe::{SubscribePacket, SubscriptionOptions, TopicFilter};
use crate::packet::unsuback::UnsubAckPacket;
use crate::packet::unsubscribe::UnsubscribePacket;
use crate::packet::{MqttPacket, Packet};
use crate::packet_id::PacketIdGenerator;
use crate::protocol::v5::properties::{Properties, PropertyId, PropertyValue};
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::session::flow_control::FlowControlManager;
use crate::session::state::OutboundReplay;
use crate::session::subscription::Subscription;
use crate::session::SessionState;
use crate::transport::{PacketIo, PacketWriter, TransportType};
use crate::types::{ConnectOptions, ConnectResult, PublishOptions};
use crate::QoS;

#[cfg(feature = "opentelemetry")]
use crate::telemetry::propagation;
#[cfg(feature = "transport-quic")]
use crate::transport::flow::{FlowFlags, FlowId};
#[cfg(feature = "transport-quic")]
use crate::transport::QuicStreamManager;
#[cfg(feature = "transport-quic")]
use crate::transport::StreamStrategy;
#[cfg(feature = "transport-quic")]
use quinn::{Connection, Endpoint};

pub use unified::{UnifiedReader, UnifiedWriter};

const SIZE_PROBE_PACKET_ID: u16 = 1;

#[cfg(feature = "transport-quic")]
use keepalive::flow_expiration_task;
use keepalive::{keepalive_task_with_writer, KeepaliveState};
#[cfg(feature = "transport-quic")]
use reader::quic_stream_acceptor_task;
use reader::{packet_reader_task_with_responses, PacketReaderContext};
use replay::{ConnectionLink, OfflineQueue, PublishPolicy, QueuedPublish, SessionReplay};
use tracking::{Completion, IdReservation, OutboundIds, OutcomeTracker, SharedIds, SharedOutcomes};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomaticReconnectLifecycle {
    Armed,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubscriptionPersistence {
    Persist,
    Skip,
}

pub(crate) type StoredSubscription = (String, SubscriptionOptions, Option<u32>, CallbackId);
pub(crate) type StoredSubscriptions = Arc<Mutex<Vec<StoredSubscription>>>;
pub(crate) type ConnectionEpoch = Arc<AtomicU64>;
const ACKNOWLEDGEMENT_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(crate) struct ReadyPublish {
    request: PublishPacket,
    packet: PublishPacket,
    reservation: Option<IdReservation>,
    epoch: u64,
}

impl ReadyPublish {
    pub(crate) fn packet_id(&self) -> Option<u16> {
        self.packet.packet_id
    }
}

#[derive(Debug)]
pub(crate) enum StagedPublish {
    Queued(PublishHandle),
    Ready(Box<ReadyPublish>),
}

pub(crate) enum Transmitted {
    Sent(Delivery),
    InFlight(InFlight),
    Detached(PublishHandle),
    Restaged(Box<ReadyPublish>),
}

pub(crate) struct InFlight {
    handle: PublishHandle,
    link: watch::Receiver<bool>,
}

impl InFlight {
    pub(crate) async fn settle(self) -> Result<PublishResult> {
        let Self { handle, mut link } = self;
        let outcome = handle.clone().outcome();
        tokio::select! {
            biased;
            outcome = outcome => match outcome {
                PublishOutcome::Delivered(delivery) => Ok(PublishResult::Sent(delivery)),
                PublishOutcome::Rejected(PublishRejection::Refused(reason_code)) => {
                    Err(MqttError::PublishFailed(reason_code))
                }
                PublishOutcome::Rejected(_) | PublishOutcome::Indeterminate(_) => {
                    Ok(PublishResult::Queued(handle))
                }
            },
            _ = link.wait_for(|alive| !*alive) => {
                tracing::debug!("Connection ended before the acknowledgement; publish stays in flight");
                Ok(PublishResult::Queued(handle))
            }
            () = tokio::time::sleep(ACKNOWLEDGEMENT_WAIT) => {
                tracing::debug!("Acknowledgement not received in time; publish stays in flight");
                Ok(PublishResult::Queued(handle))
            }
        }
    }
}

pub struct DirectClientInner {
    pub writer: Option<Arc<tokio::sync::Mutex<UnifiedWriter>>>,
    #[cfg(feature = "transport-quic")]
    pub quic_connection: Option<Arc<Connection>>,
    #[cfg(feature = "transport-quic")]
    pub quic_endpoint: Option<Endpoint>,
    #[cfg(feature = "transport-quic")]
    pub stream_strategy: Option<StreamStrategy>,
    #[cfg(feature = "transport-quic")]
    pub quic_datagrams_enabled: bool,
    #[cfg(feature = "transport-quic")]
    pub quic_stream_manager: Option<Arc<QuicStreamManager>>,
    pub session: Arc<tokio::sync::RwLock<SessionState>>,
    pub connected: Arc<AtomicBool>,
    pub connection_event_callbacks:
        Arc<tokio::sync::RwLock<Vec<crate::client::ConnectionEventCallback>>>,
    pub connection_epoch: ConnectionEpoch,
    pub callback_manager: Arc<CallbackManager>,
    /// Registry of `subscribe_with_ack` callbacks (single-owner, token-bearing).
    pub ack_callbacks: Arc<AckCallbackManager>,
    /// Connection-stable writer of deferred acknowledgements. Outlives reconnects.
    pub ack_dispatcher: Arc<AckDispatcher>,
    pub packet_reader_handle: Option<JoinHandle<()>>,
    pub keepalive_handle: Option<JoinHandle<()>>,
    #[cfg(feature = "transport-quic")]
    pub quic_stream_acceptor_handle: Option<JoinHandle<()>>,
    #[cfg(feature = "transport-quic")]
    pub flow_expiration_handle: Option<JoinHandle<()>>,
    pub options: ConnectOptions,
    pub packet_id_generator: PacketIdGenerator,
    pub pending_subacks: Arc<Mutex<HashMap<u16, oneshot::Sender<SubAckPacket>>>>,
    pub pending_unsubacks: Arc<Mutex<HashMap<u16, oneshot::Sender<UnsubAckPacket>>>>,
    pub reconnect_attempt: u32,
    pub last_address: Option<String>,
    pub automatic_reconnect_lifecycle: AutomaticReconnectLifecycle,
    pub server_redirect: Option<String>,
    pub queued_messages: Arc<Mutex<OfflineQueue>>,
    outbound_ids: SharedIds,
    publish_outcomes: SharedOutcomes,
    outbound_transfer: Arc<tokio::sync::Mutex<()>>,
    connection_alive: Option<Arc<watch::Sender<bool>>>,
    send_flow: Arc<tokio::sync::RwLock<FlowControlManager>>,
    pub stored_subscriptions: StoredSubscriptions,
    pub stored_ack_subscriptions: StoredSubscriptions,
    pub queue_on_disconnect: bool,
    pub server_max_qos: Arc<Mutex<Option<u8>>>,
    pub server_retain_available: Arc<AtomicBool>,
    pub auth_handler: Option<Arc<dyn AuthHandler>>,
    pub auth_method: Option<String>,
    pub keepalive_state: Arc<Mutex<KeepaliveState>>,
    pub negotiated_keep_alive_secs: AtomicU64,
    server_capabilities: outbound::ServerCapabilities,
    #[cfg(feature = "transport-quic")]
    pub cached_quic_client_config: Option<quinn::ClientConfig>,
    #[cfg(feature = "transport-quic")]
    pub zero_rtt_accepted: bool,
}

impl DirectClientInner {
    pub fn new(options: ConnectOptions) -> Self {
        let session_state = SessionState::new(
            options.client_id.clone(),
            options.session_config.clone(),
            options.clean_start,
        );
        let send_flow = Arc::clone(session_state.flow_control());
        let session = Arc::new(tokio::sync::RwLock::new(session_state));

        let queue_on_disconnect = !options.clean_start;
        let auth_method = options.properties.authentication_method.clone();
        let initial_keep_alive_secs = options.keep_alive.as_secs();
        let ack_dispatcher = Arc::new(AckDispatcher::new(Arc::clone(&session)));
        let ack_callbacks = Arc::new(AckCallbackManager::new());

        Self {
            writer: None,
            #[cfg(feature = "transport-quic")]
            quic_connection: None,
            #[cfg(feature = "transport-quic")]
            quic_endpoint: None,
            #[cfg(feature = "transport-quic")]
            stream_strategy: None,
            #[cfg(feature = "transport-quic")]
            quic_datagrams_enabled: false,
            #[cfg(feature = "transport-quic")]
            quic_stream_manager: None,
            session,
            connected: Arc::new(AtomicBool::new(false)),
            connection_event_callbacks: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            connection_epoch: Arc::new(AtomicU64::new(0)),
            callback_manager: Arc::new(CallbackManager::new()),
            ack_callbacks,
            ack_dispatcher,
            packet_reader_handle: None,
            keepalive_handle: None,
            #[cfg(feature = "transport-quic")]
            quic_stream_acceptor_handle: None,
            #[cfg(feature = "transport-quic")]
            flow_expiration_handle: None,
            options,
            packet_id_generator: PacketIdGenerator::new(),
            pending_subacks: Arc::new(Mutex::new(HashMap::new())),
            pending_unsubacks: Arc::new(Mutex::new(HashMap::new())),
            reconnect_attempt: 0,
            last_address: None,
            automatic_reconnect_lifecycle: AutomaticReconnectLifecycle::Armed,
            server_redirect: None,
            queued_messages: Arc::new(Mutex::new(OfflineQueue::default())),
            outbound_ids: Arc::new(Mutex::new(OutboundIds::default())),
            publish_outcomes: Arc::new(Mutex::new(OutcomeTracker::default())),
            outbound_transfer: Arc::new(tokio::sync::Mutex::new(())),
            connection_alive: None,
            send_flow,
            stored_subscriptions: Arc::new(Mutex::new(Vec::new())),
            stored_ack_subscriptions: Arc::new(Mutex::new(Vec::new())),
            queue_on_disconnect,
            server_max_qos: Arc::new(Mutex::new(None)),
            server_retain_available: Arc::new(AtomicBool::new(true)),
            auth_handler: None,
            auth_method,
            keepalive_state: Arc::new(Mutex::new(KeepaliveState::default())),
            negotiated_keep_alive_secs: AtomicU64::new(initial_keep_alive_secs),
            server_capabilities: outbound::ServerCapabilities::default(),
            #[cfg(feature = "transport-quic")]
            cached_quic_client_config: None,
            #[cfg(feature = "transport-quic")]
            zero_rtt_accepted: false,
        }
    }

    pub fn set_auth_handler(&mut self, handler: impl AuthHandler + 'static) {
        self.auth_handler = Some(Arc::new(handler));
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub fn negotiated_keep_alive(&self) -> Duration {
        Duration::from_secs(self.negotiated_keep_alive_secs.load(Ordering::Relaxed))
    }

    fn configured_keep_alive_u16(&self) -> u16 {
        let requested = self.options.keep_alive.as_secs();
        u16::try_from(requested).unwrap_or_else(|_| {
            tracing::warn!(
                "Configured keep-alive {}s exceeds the u16 wire range; clamping to {}s",
                requested,
                u16::MAX,
            );
            u16::MAX
        })
    }

    fn apply_negotiated_keep_alive(&self, server_value: Option<u16>) {
        let effective = server_value.map_or_else(
            || self.configured_keep_alive_u16(),
            |v| {
                tracing::debug!(
                    "Server overrode keep-alive: requested={}s, negotiated={}s",
                    self.options.keep_alive.as_secs(),
                    v,
                );
                v
            },
        );
        self.negotiated_keep_alive_secs
            .store(u64::from(effective), Ordering::Relaxed);
    }

    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::SeqCst);
    }

    pub(crate) fn advance_connection_epoch(&self) -> u64 {
        self.connection_epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    async fn reset_connection_runtime(&mut self, reason: &[u8]) {
        tracing::debug!(
            reason = %String::from_utf8_lossy(reason),
            "resetting connection runtime"
        );
        self.set_connected(false);
        if let Some(alive) = self.connection_alive.take() {
            alive.send_replace(false);
        }
        drop(self.outbound_transfer.lock().await);
        self.stop_background_tasks().await;
        self.keepalive_state.lock().reset();

        #[cfg(feature = "transport-quic")]
        if let Some(manager) = self.quic_stream_manager.take() {
            manager.close_all_streams().await;
        }

        self.writer = None;
        self.ack_dispatcher.clear_writer().await;

        #[cfg(feature = "transport-quic")]
        if let Some(conn) = self.quic_connection.take() {
            conn.close(
                quinn::VarInt::from_u32(mqtt5_protocol::QuicConnectionCode::NoError.code()),
                reason,
            );
        }
        #[cfg(feature = "transport-quic")]
        if let Some(endpoint) = self.quic_endpoint.take() {
            tokio::spawn(async move {
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(2), endpoint.wait_idle())
                        .await;
            });
        }
        #[cfg(feature = "transport-quic")]
        {
            self.stream_strategy = None;
            self.quic_datagrams_enabled = false;
        }
    }

    pub fn is_queue_on_disconnect(&self) -> bool {
        self.queue_on_disconnect
    }

    pub fn set_queue_on_disconnect(&mut self, enabled: bool) {
        self.queue_on_disconnect = enabled;
    }
}

impl DirectClientInner {
    async fn handle_connect_auth(
        &self,
        auth: AuthPacket,
        transport: &mut TransportType,
    ) -> Result<()> {
        tracing::debug!("CLIENT: Got AUTH with reason code: {:?}", auth.reason_code);

        match auth.reason_code {
            ReasonCode::ContinueAuthentication => {
                let method = self.auth_method.clone().ok_or_else(|| {
                    MqttError::ProtocolError(
                        "AUTH received but CONNECT carried no Authentication Method".to_string(),
                    )
                })?;
                let handler = self
                    .auth_handler
                    .as_ref()
                    .ok_or(MqttError::AuthenticationFailed)?;

                let auth_method = auth.authentication_method().unwrap_or("");
                let auth_data = auth.authentication_data();

                let response = handler.handle_challenge(auth_method, auth_data).await?;

                match response {
                    AuthResponse::Continue(data) => {
                        let auth_packet = AuthPacket::continue_authentication(method, Some(data))?;
                        transport.write_packet(Packet::Auth(auth_packet)).await?;
                    }
                    AuthResponse::Success => {
                        tracing::debug!(
                            "CLIENT: Auth handler indicated success, waiting for server response"
                        );
                    }
                    AuthResponse::Abort(reason) => {
                        tracing::warn!("CLIENT: Auth aborted: {}", reason);
                        return Err(MqttError::AuthenticationFailed);
                    }
                }
            }
            ReasonCode::Success => {
                tracing::debug!("CLIENT: AUTH success, waiting for CONNACK");
            }
            _ => {
                tracing::warn!(
                    "CLIENT: AUTH failed with reason code: {:?}",
                    auth.reason_code
                );
                return Err(MqttError::AuthenticationFailed);
            }
        }
        Ok(())
    }

    async fn wait_for_connack(
        &self,
        transport: &mut TransportType,
    ) -> Result<crate::packet::connack::ConnAckPacket> {
        loop {
            let packet = transport
                .read_packet(self.options.protocol_version.as_u8())
                .await?;

            match packet {
                Packet::Auth(auth) => {
                    self.handle_connect_auth(auth, transport).await?;
                }
                Packet::ConnAck(connack) => {
                    tracing::debug!(
                        "CLIENT: Got CONNACK with reason code: {:?}",
                        connack.reason_code
                    );
                    return Ok(connack);
                }
                _ => {
                    return Err(MqttError::ProtocolError(
                        "Expected CONNACK or AUTH".to_string(),
                    ));
                }
            }
        }
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn connect(&mut self, mut transport: TransportType) -> Result<ConnectResult> {
        self.reset_connection_runtime(b"reconnect").await;

        let connect_packet = self.build_connect_packet().await;

        transport
            .write_packet(Packet::Connect(Box::new(connect_packet)))
            .await?;

        tracing::debug!("CLIENT: Waiting for CONNACK or AUTH");

        let connack = self.wait_for_connack(&mut transport).await?;

        if connack.reason_code == ReasonCode::UseAnotherServer
            || connack.reason_code == ReasonCode::ServerMoved
        {
            self.server_redirect = connack.properties.get_server_reference().map(String::from);
            return Err(MqttError::ConnectionRefused(connack.reason_code));
        }

        if connack.reason_code != ReasonCode::Success {
            return Err(MqttError::ConnectionRefused(connack.reason_code));
        }

        if connack.session_present && !self.holds_session_state() {
            return Err(Self::reject_unexpected_session_present(&mut transport).await);
        }

        let receive_maximum = self.apply_server_capabilities(&connack).await?;
        self.apply_negotiated_capabilities(&connack).await;
        let replay_items = self.session.read().await.outbound_replay().await;
        let replay_slots = self.reset_send_quota(receive_maximum, &replay_items).await;

        let protocol_version = self.options.protocol_version.as_u8();
        let (reader, writer) = match transport {
            TransportType::Tcp(tcp) => {
                let (r, w) = tcp.into_split()?;
                (
                    UnifiedReader::tcp(r, protocol_version),
                    UnifiedWriter::Tcp(w),
                )
            }
            TransportType::Tls(tls) => {
                let (r, w) = (*tls).into_split()?;
                (
                    UnifiedReader::tls(r, protocol_version),
                    UnifiedWriter::Tls(w),
                )
            }
            #[cfg(feature = "transport-websocket")]
            TransportType::WebSocket(ws) => {
                let (r, w) = (*ws).into_split()?;
                (
                    UnifiedReader::websocket(r, protocol_version),
                    UnifiedWriter::WebSocket(w),
                )
            }
            #[cfg(feature = "transport-quic")]
            TransportType::Quic(quic) => {
                let split = (*quic).into_split()?;
                let conn_arc = Arc::new(split.connection);
                self.quic_connection = Some(conn_arc.clone());
                self.quic_endpoint = Some(split.endpoint);
                self.stream_strategy = Some(split.strategy);
                self.quic_datagrams_enabled = split.datagrams_enabled;
                self.zero_rtt_accepted = split.zero_rtt_accepted;
                self.cached_quic_client_config = split.client_config;
                let effective_flow_headers =
                    split.flow_headers_enabled && split.negotiated_mqtt_next;
                self.quic_stream_manager = Some(Arc::new(
                    QuicStreamManager::new(Arc::clone(&conn_arc), split.strategy)
                        .with_flow_headers(effective_flow_headers)
                        .with_flow_expire_interval(split.flow_expire_interval)
                        .with_flow_flags(split.flow_flags),
                ));
                (
                    UnifiedReader::quic(split.recv, protocol_version),
                    UnifiedWriter::QuicControl(split.send, conn_arc),
                )
            }
        };

        let reader = reader.with_maximum_packet_size(self.options.properties.maximum_packet_size);
        let connection_epoch = self.advance_connection_epoch();
        let writer_arc = Arc::new(tokio::sync::Mutex::new(writer));
        self.ack_dispatcher
            .set_writer(Arc::clone(&writer_arc))
            .await;
        let replay_writer = Arc::downgrade(&writer_arc);
        self.writer = Some(writer_arc);
        self.connection_alive = Some(Arc::new(watch::channel(true).0));
        self.set_connected(true);

        tracing::debug!("Starting background tasks (packet reader and keepalive)");
        self.start_background_tasks(reader, connection_epoch)?;
        tracing::debug!("Background tasks started successfully");

        if let Some(slots) = replay_slots {
            tokio::spawn(
                self.session_replay(replay_items, slots, replay_writer, connection_epoch)
                    .run(),
            );
        }

        Ok(ConnectResult {
            session_present: connack.session_present,
        })
    }

    fn session_replay(
        &self,
        items: Vec<OutboundReplay>,
        slots: Arc<tokio::sync::Semaphore>,
        writer: std::sync::Weak<tokio::sync::Mutex<UnifiedWriter>>,
        epoch: u64,
    ) -> SessionReplay {
        SessionReplay {
            items,
            slots,
            session: Arc::clone(&self.session),
            writer,
            queued: Arc::clone(&self.queued_messages),
            policy: self.publish_policy(),
            outcomes: Arc::clone(&self.publish_outcomes),
            ids: Arc::clone(&self.outbound_ids),
            link: ConnectionLink {
                epoch,
                current_epoch: Arc::clone(&self.connection_epoch),
                connected: Arc::clone(&self.connected),
                transfer: Arc::clone(&self.outbound_transfer),
                alive: self
                    .connection_alive
                    .as_ref()
                    .map_or_else(|| watch::channel(false).1, |alive| alive.subscribe()),
            },
        }
    }

    fn holds_session_state(&self) -> bool {
        !self.options.clean_start
            && (self.connection_epoch.load(Ordering::SeqCst) > 0
                || self.options.resume_existing_session)
    }

    async fn apply_server_capabilities(
        &mut self,
        connack: &crate::packet::connack::ConnAckPacket,
    ) -> Result<u16> {
        if !connack.session_present {
            self.discard_session_state().await;
        }
        self.adopt_assigned_client_identifier(connack).await;

        if let Some(max_qos) = connack.properties.get_maximum_qos() {
            *self.server_max_qos.lock() = Some(max_qos);
            tracing::debug!("Server maximum QoS: {}", max_qos);
        } else {
            *self.server_max_qos.lock() = None;
        }
        self.server_retain_available.store(
            !matches!(
                connack.properties.get(PropertyId::RetainAvailable),
                Some(PropertyValue::Byte(0))
            ),
            Ordering::SeqCst,
        );

        self.apply_negotiated_keep_alive(connack.properties.get_server_keep_alive());

        self.apply_negotiated_packet_sizes(connack).await
    }

    async fn reject_unexpected_session_present(transport: &mut TransportType) -> MqttError {
        tracing::warn!(
            "CONNACK reported Session Present=1 but the client holds no session state; closing the connection"
        );
        let disconnect = crate::packet::disconnect::DisconnectPacket {
            reason_code: ReasonCode::ProtocolError,
            properties: Properties::default(),
        };
        if let Err(e) = transport.write_packet(Packet::Disconnect(disconnect)).await {
            tracing::debug!("Failed to send DISCONNECT for unexpected Session Present: {e}");
        }
        MqttError::ProtocolError(
            "CONNACK Session Present=1 but the client holds no session state".to_string(),
        )
    }

    async fn discard_session_state(&self) {
        let session = self.session.write().await;
        self.ack_dispatcher.discard_pending();
        let unacknowledged = session.outbound_replay().await;
        session.discard_outbound_state().await;
        self.requeue_lost_session(unacknowledged);
        session.flow_control().read().await.clear_inbound().await;
        if session.clear_all_inbound_state().await {
            if self.options.deferred_ack {
                tracing::warn!(
                    "Reconnected with session_present=0; cleared stale inbound QoS 2 \
                     de-duplication state. Any outstanding AckTokens are now stale because the \
                     broker no longer holds the session that delivered their messages."
                );
            } else {
                tracing::debug!(
                    "Reconnected with session_present=0; cleared stale inbound QoS 2 de-duplication state"
                );
            }
        }
    }

    fn requeue_lost_session(&self, unacknowledged: Vec<OutboundReplay>) {
        self.outbound_ids.lock().release_quarantine();
        let resume_requested = !self.options.clean_start;
        let lost = if resume_requested {
            IndeterminateReason::SessionLost
        } else {
            IndeterminateReason::SessionDiscarded
        };
        let mut outcomes = self.publish_outcomes.lock();
        let mut resend = Vec::new();
        for item in unacknowledged {
            let (packet_id, requeue) = match item {
                OutboundReplay::Publish(publish) => {
                    let requeue = publish.qos == QoS::AtLeastOnce && resume_requested;
                    (publish.packet_id, requeue.then_some(publish))
                }
                OutboundReplay::PubRel(packet_id) => {
                    if let Some(completion) = outcomes.take(packet_id) {
                        tracing::debug!(
                            packet_id,
                            "Session not resumed after PUBREC; the server owns the message"
                        );
                        completion.delivered(Delivery::ExactlyOnce { packet_id });
                    }
                    continue;
                }
            };
            let Some(packet_id) = packet_id else {
                continue;
            };
            let completion = outcomes.take(packet_id);
            let reservation = requeue
                .as_ref()
                .and_then(|_| IdReservation::claim(&self.outbound_ids, packet_id));
            if let (Some(publish), Some(reservation)) = (requeue, reservation) {
                tracing::debug!(
                    packet_id,
                    "Session not resumed; unacknowledged QoS 1 PUBLISH re-queued"
                );
                let completion = completion.map(|mut completion| {
                    completion.mark_resent();
                    completion
                });
                resend.push(QueuedPublish::new(
                    PublishPacket {
                        dup: false,
                        ..publish
                    },
                    reservation,
                    completion,
                ));
            } else if let Some(completion) = completion {
                tracing::warn!(
                    packet_id,
                    ?lost,
                    "Session not resumed; unacknowledged outbound exchange dropped"
                );
                completion.indeterminate(lost);
            }
        }
        outcomes.abandon_all(lost);
        drop(outcomes);
        self.queued_messages.lock().push_front_in_order(resend);
    }

    async fn adopt_assigned_client_identifier(
        &mut self,
        connack: &crate::packet::connack::ConnAckPacket,
    ) {
        if let Some(PropertyValue::Utf8String(assigned)) =
            connack.properties.get(PropertyId::AssignedClientIdentifier)
        {
            tracing::debug!(client_id = %assigned, "Adopting server assigned client identifier");
            self.session.write().await.set_client_id(assigned.clone());
            self.options.client_id.clone_from(assigned);
        }
    }

    async fn reset_send_quota(
        &self,
        receive_maximum: u16,
        replay_items: &[OutboundReplay],
    ) -> Option<Arc<tokio::sync::Semaphore>> {
        let retained_in_flight: Vec<u16> = replay_items
            .iter()
            .filter_map(|item| match item {
                OutboundReplay::PubRel(packet_id) => Some(*packet_id),
                OutboundReplay::Publish(_) => None,
            })
            .collect();
        let replay = !replay_items.is_empty() || !self.queued_messages.lock().is_empty();
        let flow = Arc::clone(self.session.read().await.flow_control());
        let mut flow = flow.write().await;
        flow.reset_for_connection(receive_maximum, &retained_in_flight, replay)
            .await
    }

    fn publish_policy(&self) -> PublishPolicy {
        PublishPolicy {
            maximum_qos: *self.server_max_qos.lock(),
            retain_available: self.server_retain_available.load(Ordering::SeqCst),
        }
    }

    async fn apply_negotiated_packet_sizes(
        &self,
        connack: &crate::packet::connack::ConnAckPacket,
    ) -> Result<u16> {
        let session = self.session.write().await;

        let receive_maximum = match connack.properties.get_receive_maximum() {
            Some(0) => {
                return Err(MqttError::ProtocolError(
                    "server advertised a Receive Maximum of 0".to_string(),
                ));
            }
            Some(server_receive_maximum) => {
                tracing::debug!("Server Receive Maximum: {}", server_receive_maximum);
                server_receive_maximum
            }
            None => 65535,
        };

        if let Some(receive_maximum) = self.options.properties.receive_maximum {
            session.set_inbound_receive_maximum(receive_maximum).await;
        }

        if let Some(max_packet_size) = self.options.properties.maximum_packet_size {
            session
                .set_client_maximum_packet_size(max_packet_size)
                .await;
        }

        match connack.properties.get_maximum_packet_size() {
            Some(server_max_packet_size) => {
                session
                    .set_server_maximum_packet_size(server_max_packet_size)
                    .await;
                tracing::debug!("Server maximum packet size: {}", server_max_packet_size);
            }
            None => session.reset_server_maximum_packet_size().await,
        }

        Ok(receive_maximum)
    }

    async fn apply_negotiated_capabilities(
        &mut self,
        connack: &crate::packet::connack::ConnAckPacket,
    ) {
        self.server_capabilities = outbound::ServerCapabilities::from_connack(connack);
        self.session
            .read()
            .await
            .set_topic_alias_maximum_out(connack.topic_alias_maximum().unwrap_or(0))
            .await;
    }

    /// # Errors
    ///
    /// Returns an error if the client is not connected, no auth handler is set,
    /// or no authentication method was used during initial connection
    pub async fn reauthenticate(&self) -> Result<()> {
        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }

        let handler = self
            .auth_handler
            .as_ref()
            .ok_or(MqttError::AuthenticationFailed)?;
        let method = self
            .auth_method
            .as_ref()
            .ok_or(MqttError::AuthenticationFailed)?;

        let initial_data = handler.initial_response(method).await?;
        let auth_packet = AuthPacket::re_authenticate(method.clone(), initial_data)?;

        let writer = self.writer.as_ref().ok_or(MqttError::NotConnected)?;
        writer
            .lock()
            .await
            .write_packet(Packet::Auth(auth_packet))
            .await?;

        tracing::debug!(
            "CLIENT: Initiated re-authentication with method: {}",
            method
        );
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn disconnect(&mut self) -> Result<()> {
        self.disconnect_with_packet(true).await
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn disconnect_with_packet(&mut self, send_disconnect: bool) -> Result<()> {
        if !self.is_connected() {
            self.reset_connection_runtime(b"disconnect").await;
            self.session
                .read()
                .await
                .flow_control()
                .read()
                .await
                .close_send_quota();
            return Err(MqttError::NotConnected);
        }

        self.set_connected(false);
        if let Some(ref writer) = self.writer {
            let disconnect = send_disconnect.then(|| {
                Packet::Disconnect(crate::packet::disconnect::DisconnectPacket::new(
                    ReasonCode::Success,
                ))
            });
            if let Err(e) = writer.lock().await.close(disconnect).await {
                tracing::debug!("Closing network connection on disconnect: {e}");
            }
        }

        self.reset_connection_runtime(b"disconnect").await;
        self.session
            .read()
            .await
            .flow_control()
            .read()
            .await
            .close_send_quota();

        Ok(())
    }

    /// # Errors
    ///
    /// Returns `RetainNotSupported` or `PacketTooLarge` when the message already
    /// violates the last known Retain Available or negotiated maximum packet size, so
    /// the publish is rejected at enqueue time instead of being accepted and then
    /// rejected when the queue is flushed on reconnect. A packet identifier is
    /// allocated only after these checks pass.
    async fn queue_publish_message(
        &self,
        topic: String,
        payload: Vec<u8>,
        options: &PublishOptions,
    ) -> Result<PublishHandle> {
        if options.retain && !self.server_retain_available.load(Ordering::SeqCst) {
            return Err(MqttError::RetainNotSupported);
        }

        let mut publish = self
            .with_aliased_topic(PublishPacket {
                topic_name: topic,
                packet_id: Some(SIZE_PROBE_PACKET_ID),
                payload: payload.into(),
                qos: options.qos,
                retain: options.retain,
                dup: false,
                properties: options.properties.clone().into(),
                protocol_version: self.options.protocol_version.as_u8(),
                stream_id: None,
            })
            .await?;

        self.check_publish_size(&publish).await?;

        let reservation = self.allocate_packet_id().await?;
        publish.packet_id = Some(reservation.packet_id());
        let (completion, handle) = Completion::new();
        self.queued_messages.lock().push_back(QueuedPublish::new(
            publish,
            reservation,
            Some(completion),
        ));
        Ok(handle)
    }

    async fn with_aliased_topic(&self, mut publish: PublishPacket) -> Result<PublishPacket> {
        if let Some(alias) = publish
            .topic_alias()
            .filter(|_| publish.topic_name.is_empty())
        {
            let session = self.session.read().await;
            let aliases = session.topic_alias_out().read().await;
            publish.topic_name = aliases
                .get_topic(alias)
                .map(str::to_string)
                .ok_or(MqttError::TopicAliasInvalid(alias))?;
        }
        Ok(publish)
    }

    async fn allocate_packet_id(&self) -> Result<IdReservation> {
        for _ in 0..u16::MAX {
            let packet_id = self
                .session
                .read()
                .await
                .allocate_packet_id(&self.packet_id_generator, |packet_id| {
                    self.pending_subacks.lock().contains_key(&packet_id)
                        || self.pending_unsubacks.lock().contains_key(&packet_id)
                        || self.outbound_ids.lock().holds(packet_id)
                })
                .await
                .ok_or(MqttError::PacketIdExhausted)?;
            if let Some(reservation) = IdReservation::claim(&self.outbound_ids, packet_id) {
                return Ok(reservation);
            }
        }
        Err(MqttError::PacketIdExhausted)
    }

    /// # Errors
    ///
    /// Returns `PacketTooLarge` if the encoded packet exceeds the negotiated
    /// maximum packet size for the current connection.
    pub(crate) async fn check_publish_size(&self, publish: &PublishPacket) -> Result<()> {
        let mut buf = bytes::BytesMut::new();
        publish.encode(&mut buf)?;
        self.session.read().await.check_packet_size(buf.len()).await
    }

    async fn check_packet_fits(&self, packet: &impl MqttPacket) -> Result<()> {
        let mut buf = bytes::BytesMut::new();
        packet.encode(&mut buf)?;
        self.session.read().await.check_packet_size(buf.len()).await
    }

    pub(crate) async fn check_unsubscribe(&self, packet: &UnsubscribePacket) -> Result<()> {
        outbound::check_unsubscribe(packet)?;
        self.check_packet_fits(packet).await
    }

    pub(super) async fn release_outbound_quota(
        session: &Arc<tokio::sync::RwLock<SessionState>>,
        packet_id: Option<u16>,
    ) {
        if let Some(pid) = packet_id {
            let session = session.read().await;
            session.complete_outbound(pid).await;
            let flow = Arc::clone(session.flow_control());
            drop(session);
            Self::release_send_quota(&flow, pid).await;
        }
    }

    async fn release_send_quota(
        flow: &Arc<tokio::sync::RwLock<FlowControlManager>>,
        packet_id: u16,
    ) {
        if let Err(e) = flow.read().await.acknowledge(packet_id).await {
            tracing::trace!(packet_id, "No send quota held: {e}");
        }
    }

    pub(crate) async fn stage_publish(
        &self,
        topic: String,
        payload: Vec<u8>,
        options: PublishOptions,
    ) -> Result<StagedPublish> {
        let protocol_version = self.options.protocol_version.as_u8();
        outbound::check_publish(&topic, &options, protocol_version)?;

        if !self.is_connected() && self.queue_on_disconnect && options.qos != QoS::AtMostOnce {
            return self
                .queue_publish_message(topic, payload, &options)
                .await
                .map(StagedPublish::Queued);
        }

        #[cfg(feature = "opentelemetry")]
        let options = {
            let mut opts = options;
            propagation::inject_trace_context(&mut opts.properties.user_properties);
            opts
        };

        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }

        let (final_payload, properties) = self.encode_payload(payload, &options)?;
        let request = PublishPacket {
            topic_name: topic,
            payload: final_payload,
            qos: options.qos,
            retain: options.retain,
            dup: false,
            packet_id: None,
            properties: if protocol_version == 5 {
                properties
            } else {
                Properties::default()
            },
            protocol_version,
            stream_id: None,
        };
        self.conform_to_connection(request)
            .await
            .map(StagedPublish::Ready)
    }

    async fn conform_to_connection(&self, request: PublishPacket) -> Result<Box<ReadyPublish>> {
        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }

        if let Some(alias) = request.topic_alias() {
            let session = self.session.read().await;
            let aliases = session.topic_alias_out().read().await;
            outbound::check_topic_alias(&aliases, &request.topic_name, alias)?;
        }

        let mut packet = self.publish_policy().conform(PublishPacket {
            packet_id: (request.qos != QoS::AtMostOnce).then_some(SIZE_PROBE_PACKET_ID),
            ..request.clone()
        })?;

        self.check_publish_size(&packet).await?;

        let reservation = if packet.qos == QoS::AtMostOnce {
            None
        } else {
            let reservation = self.allocate_packet_id().await?;
            packet.packet_id = Some(reservation.packet_id());
            Some(reservation)
        };

        Ok(Box::new(ReadyPublish {
            request,
            packet,
            reservation,
            epoch: self.connection_epoch.load(Ordering::SeqCst),
        }))
    }

    pub(crate) async fn transmit_publish(
        &self,
        ready: Box<ReadyPublish>,
        claim: Option<u64>,
    ) -> Result<Transmitted> {
        let packet_id = ready.packet_id();
        let flow = Arc::clone(self.session.read().await.flow_control());
        let quota_generation = flow.read().await.quota_generation();
        let claimed = packet_id.filter(|_| claim == Some(quota_generation));

        if !self.is_connected() {
            if let Some(pid) = claimed {
                Self::release_send_quota(&flow, pid).await;
            }
            return Err(MqttError::NotConnected);
        }

        if ready.epoch != self.connection_epoch.load(Ordering::SeqCst) {
            if let Some(pid) = claimed {
                Self::release_send_quota(&flow, pid).await;
            }
            tracing::debug!(
                packet_id = ?packet_id,
                "Connection changed before publish was sent; conforming it to the current connection"
            );
            return self
                .conform_to_connection(ready.request)
                .await
                .map(Transmitted::Restaged);
        }

        let ReadyPublish {
            packet: publish,
            reservation,
            ..
        } = *ready;
        let qos = publish.qos;

        let in_flight = match packet_id {
            Some(pid) if qos != QoS::AtMostOnce => {
                let stored = match self.with_aliased_topic(publish.clone()).await {
                    Ok(stored) => {
                        self.session
                            .read()
                            .await
                            .store_unacked_publish(stored)
                            .await
                    }
                    Err(e) => Err(e),
                };
                if let Err(e) = stored {
                    Self::release_send_quota(&flow, pid).await;
                    return Err(e);
                }
                let (completion, handle) = Completion::new();
                self.publish_outcomes.lock().track(pid, qos, completion);
                Some(handle)
            }
            _ => None,
        };
        drop(reservation);

        if publish.payload.len() > 10000 {
            tracing::debug!(
                topic = %publish.topic_name,
                payload_len = publish.payload.len(),
                packet_id = ?packet_id,
                qos = ?qos,
                "Sending large PUBLISH packet"
            );
        }

        let alias_mapping = publish
            .topic_alias()
            .filter(|_| !publish.topic_name.is_empty())
            .map(|alias| (alias, publish.topic_name.clone()));
        let link = self
            .connection_alive
            .as_ref()
            .map_or_else(|| watch::channel(false).1, |alive| alive.subscribe());
        let written = self.send_publish_packet(publish).await;
        if let Some((alias, alias_topic)) = alias_mapping.filter(|_| written.is_ok()) {
            self.record_outbound_topic_alias(alias, &alias_topic).await;
        }
        match (written, in_flight) {
            (Ok(()), None) => Ok(Transmitted::Sent(Delivery::of(qos, packet_id))),
            (Ok(()), Some(handle)) => Ok(Transmitted::InFlight(InFlight { handle, link })),
            (Err(e), None) => Err(e),
            (Err(e), Some(handle)) => {
                tracing::debug!(
                    packet_id = ?packet_id,
                    error = %e,
                    "Stored PUBLISH could not be written; it is re-sent with the session"
                );
                Ok(Transmitted::Detached(handle))
            }
        }
    }

    async fn record_outbound_topic_alias(&self, alias: u16, topic: &str) {
        let session = self.session.read().await;
        let mut aliases = session.topic_alias_out().write().await;
        if let Err(e) = aliases.register_alias(alias, topic) {
            tracing::warn!(alias, topic, error = %e, "outbound Topic Alias not recorded");
        }
    }

    fn encode_payload(
        &self,
        payload: Vec<u8>,
        options: &PublishOptions,
    ) -> Result<(bytes::Bytes, Properties)> {
        let (final_payload, codec_content_type) = if options.skip_codec {
            (payload.into(), None)
        } else if let Some(ref registry) = self.options.codec_registry {
            registry.encode_with_default(&payload)?
        } else {
            (payload.into(), None)
        };

        let mut properties: Properties = options.properties.clone().into();
        if let Some(ct) = codec_content_type.filter(|_| properties.get_content_type().is_none()) {
            properties.set_content_type(ct);
        }
        Ok((final_payload, properties))
    }

    async fn send_publish_packet(&self, publish: PublishPacket) -> Result<()> {
        #[cfg(feature = "transport-quic")]
        {
            let qos = publish.qos;
            if qos == QoS::AtMostOnce && self.datagrams_available() {
                if let Some(max_size) = self.max_datagram_size() {
                    let overhead = 5 + publish.topic_name.len();
                    if publish.payload.len() + overhead <= max_size {
                        let mut buf = bytes::BytesMut::new();
                        crate::transport::packet_io::encode_packet_to_buffer(
                            &Packet::Publish(publish.clone()),
                            &mut buf,
                        )?;
                        if buf.len() <= max_size && self.send_datagram(buf.freeze()).is_ok() {
                            tracing::debug!(
                                topic = %publish.topic_name,
                                payload_len = publish.payload.len(),
                                "Sent QoS 0 PUBLISH via QUIC datagram"
                            );
                            return Ok(());
                        }
                    }
                }
            }

            if let Some(manager) = &self.quic_stream_manager {
                match manager.strategy() {
                    StreamStrategy::DataPerPublish => {
                        tracing::debug!(
                            topic = %publish.topic_name,
                            qos = ?qos,
                            "Using dedicated QUIC stream for PUBLISH (DataPerPublish)"
                        );
                        manager
                            .send_packet_on_stream(Packet::Publish(publish))
                            .await?;
                        return Ok(());
                    }
                    StreamStrategy::ControlOnly => {}
                    topic_strategy => {
                        tracing::debug!(
                            topic = %publish.topic_name,
                            qos = ?qos,
                            strategy = ?topic_strategy,
                            "Using topic-specific QUIC stream for PUBLISH"
                        );
                        manager
                            .send_on_topic_stream(
                                publish.topic_name.clone(),
                                Packet::Publish(publish),
                            )
                            .await?;
                        return Ok(());
                    }
                }
            }
        }

        let writer = self.writer.as_ref().ok_or(MqttError::NotConnected)?;
        writer
            .lock()
            .await
            .write_packet(Packet::Publish(publish))
            .await?;
        Ok(())
    }

    #[cfg(feature = "transport-quic")]
    fn topic_stream_manager(&self) -> Option<&Arc<QuicStreamManager>> {
        self.quic_stream_manager.as_ref().filter(|manager| {
            !matches!(
                manager.strategy(),
                StreamStrategy::ControlOnly | StreamStrategy::DataPerPublish
            )
        })
    }

    #[cfg(feature = "transport-quic")]
    async fn unsubscribe_data_flow_manager(
        &self,
        packet: &UnsubscribePacket,
    ) -> Option<&Arc<QuicStreamManager>> {
        let [filter] = packet.filters.as_slice() else {
            return None;
        };
        let manager = self.topic_stream_manager()?;
        manager.get_flow_id_for_topic(filter).await.map(|_| manager)
    }

    #[cfg(feature = "transport-quic")]
    fn subscribe_data_flow_manager(
        &self,
        packet: &SubscribePacket,
    ) -> Option<&Arc<QuicStreamManager>> {
        if packet.filters.len() != 1 {
            return None;
        }
        self.topic_stream_manager()
    }

    #[cfg(feature = "transport-quic")]
    fn datagrams_available(&self) -> bool {
        self.quic_datagrams_enabled
            && self
                .quic_connection
                .as_ref()
                .and_then(|c| c.max_datagram_size())
                .is_some()
    }

    #[cfg(feature = "transport-quic")]
    fn max_datagram_size(&self) -> Option<usize> {
        if !self.quic_datagrams_enabled {
            return None;
        }
        self.quic_connection
            .as_ref()
            .and_then(|c| c.max_datagram_size())
    }

    #[cfg(feature = "transport-quic")]
    fn send_datagram(&self, data: bytes::Bytes) -> Result<()> {
        let conn = self
            .quic_connection
            .as_ref()
            .ok_or(MqttError::NotConnected)?;
        conn.send_datagram(data)
            .map_err(|e| MqttError::ConnectionError(format!("Datagram send failed: {e}")))
    }

    fn create_subscription_from_filter(
        filter: &TopicFilter,
        reason_code: SubAckReasonCode,
    ) -> Option<Subscription> {
        match &reason_code {
            SubAckReasonCode::GrantedQoS0 => Some(Subscription {
                topic_filter: filter.filter.clone(),
                options: SubscriptionOptions {
                    qos: QoS::AtMostOnce,
                    no_local: filter.options.no_local,
                    retain_as_published: filter.options.retain_as_published,
                    retain_handling: filter.options.retain_handling,
                },
            }),
            SubAckReasonCode::GrantedQoS1 => Some(Subscription {
                topic_filter: filter.filter.clone(),
                options: SubscriptionOptions {
                    qos: QoS::AtLeastOnce,
                    no_local: filter.options.no_local,
                    retain_as_published: filter.options.retain_as_published,
                    retain_handling: filter.options.retain_handling,
                },
            }),
            SubAckReasonCode::GrantedQoS2 => Some(Subscription {
                topic_filter: filter.filter.clone(),
                options: SubscriptionOptions {
                    qos: QoS::ExactlyOnce,
                    no_local: filter.options.no_local,
                    retain_as_published: filter.options.retain_as_published,
                    retain_handling: filter.options.retain_handling,
                },
            }),
            _ => None,
        }
    }

    async fn wait_for_suback(
        &self,
        rx: oneshot::Receiver<SubAckPacket>,
        packet_id: u16,
    ) -> Result<SubAckPacket> {
        let timeout = Duration::from_secs(10);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(suback)) => Ok(suback),
            Ok(Err(_)) => Err(MqttError::ProtocolError(
                "SUBACK channel closed".to_string(),
            )),
            Err(_) => {
                self.pending_subacks.lock().remove(&packet_id);
                Err(MqttError::Timeout)
            }
        }
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn subscribe_with_callback(
        &self,
        packet: SubscribePacket,
        callback_id: CallbackId,
    ) -> Result<Vec<(u16, QoS)>> {
        self.subscribe_with_callback_internal(packet, callback_id, SubscriptionPersistence::Persist)
            .await
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub(crate) async fn subscribe_with_callback_internal(
        &self,
        packet: SubscribePacket,
        callback_id: CallbackId,
        persistence: SubscriptionPersistence,
    ) -> Result<Vec<(u16, QoS)>> {
        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }

        self.server_capabilities.check_subscribe(&packet)?;
        self.check_packet_fits(&packet).await?;

        let writer = self.writer.as_ref().ok_or(MqttError::NotConnected)?;

        let reservation = self.allocate_packet_id().await?;
        let packet_id = reservation.packet_id();
        let mut packet = packet;
        packet.packet_id = packet_id;

        let (tx, rx) = oneshot::channel();
        self.pending_subacks.lock().insert(packet_id, tx);
        drop(reservation);

        maybe_store_subscriptions(
            &self.stored_subscriptions,
            &packet.filters,
            packet.properties.get_subscription_identifier(),
            callback_id,
            persistence,
        );

        #[cfg(feature = "transport-quic")]
        let sent_on_flow = if let Some(manager) = self.subscribe_data_flow_manager(&packet) {
            let topic = packet.filters[0].filter.clone();
            manager
                .send_on_topic_stream(topic, Packet::Subscribe(packet.clone()))
                .await?;
            true
        } else {
            false
        };
        #[cfg(not(feature = "transport-quic"))]
        let sent_on_flow = false;

        if !sent_on_flow {
            writer
                .lock()
                .await
                .write_packet(Packet::Subscribe(packet.clone()))
                .await?;
        }

        let suback = self.wait_for_suback(rx, packet_id).await?;

        for (filter, reason_code) in packet.filters.iter().zip(suback.reason_codes.iter()) {
            if let Some(subscription) = Self::create_subscription_from_filter(filter, *reason_code)
            {
                let recorded = self
                    .session
                    .write()
                    .await
                    .add_subscription(filter.filter.clone(), subscription)
                    .await;
                if let Err(e) = recorded {
                    tracing::warn!(filter = %filter.filter, error = %e, "subscription not recorded in session");
                }
            }
        }

        let mut results: Vec<(u16, QoS)> = Vec::with_capacity(suback.reason_codes.len());

        for rc in &suback.reason_codes {
            if let Some(qos) = rc.granted_qos() {
                results.push((packet_id, qos));
            } else {
                return Err(MqttError::SubscriptionDenied(*rc));
            }
        }

        Ok(results)
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn unsubscribe(&self, packet: UnsubscribePacket) -> Result<()> {
        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }

        self.check_unsubscribe(&packet).await?;

        let writer = self.writer.as_ref().ok_or(MqttError::NotConnected)?;

        let reservation = self.allocate_packet_id().await?;
        let packet_id = reservation.packet_id();
        let mut packet = packet;
        packet.packet_id = packet_id;

        let (tx, rx) = oneshot::channel();
        self.pending_unsubacks.lock().insert(packet_id, tx);
        drop(reservation);

        {
            let mut stored = self.stored_subscriptions.lock();
            for topic in &packet.filters {
                stored.retain(|(stored_topic, _, _, _)| stored_topic != topic);
            }
        }

        #[cfg(feature = "transport-quic")]
        let sent_on_flow = if let Some(manager) = self.unsubscribe_data_flow_manager(&packet).await
        {
            let topic = packet.filters[0].clone();
            manager
                .send_on_topic_stream(topic, Packet::Unsubscribe(packet.clone()))
                .await?;
            true
        } else {
            false
        };
        #[cfg(not(feature = "transport-quic"))]
        let sent_on_flow = false;

        if !sent_on_flow {
            writer
                .lock()
                .await
                .write_packet(Packet::Unsubscribe(packet.clone()))
                .await?;
        }

        let timeout = Duration::from_secs(10);
        let unsuback = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(unsuback)) => unsuback,
            Ok(Err(_)) => {
                return Err(MqttError::ProtocolError(
                    "UNSUBACK channel closed".to_string(),
                ))
            }
            Err(_) => {
                self.pending_unsubacks.lock().remove(&packet_id);
                return Err(MqttError::Timeout);
            }
        };

        if unsuback.packet_id != packet_id {
            return Err(MqttError::ProtocolError(format!(
                "UNSUBACK packet ID mismatch: expected {}, got {}",
                packet_id, unsuback.packet_id
            )));
        }

        for filter in packet.filters {
            let removed = self
                .session
                .write()
                .await
                .remove_subscription(&filter)
                .await;
            if let Err(e) = removed {
                tracing::warn!(filter = %filter, error = %e, "subscription not removed from session");
            }
        }

        Ok(())
    }

    pub(crate) async fn build_connect_packet(&self) -> ConnectPacket {
        let session = self.session.read().await;

        let mut properties = Properties::default();

        if let Some(val) = self.options.properties.session_expiry_interval {
            properties.set_session_expiry_interval(val);
        }
        if let Some(val) = self.options.properties.receive_maximum {
            properties.set_receive_maximum(val);
        }
        if let Some(val) = self.options.properties.maximum_packet_size {
            properties.set_maximum_packet_size(val);
        }
        if let Some(val) = self.options.properties.topic_alias_maximum {
            properties.set_topic_alias_maximum(val);
        }
        if let Some(val) = self.options.properties.request_response_information {
            properties.set_request_response_information(val);
        }
        if let Some(val) = self.options.properties.request_problem_information {
            properties.set_request_problem_information(val);
        }
        for (key, value) in &self.options.properties.user_properties {
            properties.add_user_property(key.clone(), value.clone());
        }
        if let Some(ref method) = self.options.properties.authentication_method {
            properties.set_authentication_method(method.clone());

            let auth_data = if let Some(ref handler) = self.auth_handler {
                match handler.initial_response(method).await {
                    Ok(data) => data,
                    Err(e) => {
                        tracing::warn!("Auth handler initial_response failed: {e}");
                        self.options.properties.authentication_data.clone()
                    }
                }
            } else {
                self.options.properties.authentication_data.clone()
            };

            if let Some(data) = auth_data {
                properties.set_authentication_data(bytes::Bytes::from(data));
            }
        }

        let will_properties = Self::build_will_properties(self.options.will.as_ref());

        ConnectPacket {
            protocol_version: self.options.protocol_version.as_u8(),
            clean_start: self.options.clean_start,
            keep_alive: self.configured_keep_alive_u16(),
            client_id: session.client_id().to_string(),
            will: self.options.will.clone(),
            username: self.options.username.clone(),
            password: self.options.password.clone(),
            properties,
            will_properties,
        }
    }

    fn build_will_properties(will: Option<&crate::types::WillMessage>) -> Properties {
        will.map_or_else(Properties::default, |w| w.properties.clone().into())
    }

    fn connection_lifecycle(
        &self,
        connection_epoch: u64,
        reader_task: Arc<Mutex<Option<tokio::task::AbortHandle>>>,
    ) -> Result<keepalive::ConnectionLifecycle> {
        Ok(keepalive::ConnectionLifecycle {
            connected: self.connected.clone(),
            connection_epoch,
            current_connection_epoch: self.connection_epoch.clone(),
            callbacks: Arc::clone(&self.connection_event_callbacks),
            closing: Arc::new(AtomicBool::new(false)),
            alive: self
                .connection_alive
                .clone()
                .ok_or(MqttError::NotConnected)?,
            flow: Arc::clone(&self.send_flow),
            reader_task,
        })
    }

    fn start_background_tasks(
        &mut self,
        reader: UnifiedReader,
        connection_epoch: u64,
    ) -> Result<()> {
        let reader_session = self.session.clone();
        let reader_callbacks = self.callback_manager.clone();
        let suback_channels = self.pending_subacks.clone();
        let unsuback_channels = self.pending_unsubacks.clone();
        let publish_outcomes = Arc::clone(&self.publish_outcomes);
        let reader_task = Arc::new(Mutex::new(None));
        let writer_for_keepalive = self.writer.as_ref().ok_or(MqttError::NotConnected)?.clone();
        let lifecycle = self.connection_lifecycle(connection_epoch, Arc::clone(&reader_task))?;

        let writer_for_reader = writer_for_keepalive.clone();
        let keepalive_state = self.keepalive_state.clone();

        let ctx = PacketReaderContext {
            session: reader_session,
            callback_manager: reader_callbacks,
            suback_channels,
            unsuback_channels,
            publish_outcomes,
            writer: writer_for_reader,
            lifecycle: lifecycle.clone(),
            #[cfg(feature = "transport-quic")]
            protocol_version: self.options.protocol_version.as_u8(),
            #[cfg(feature = "transport-quic")]
            maximum_packet_size: self
                .options
                .properties
                .maximum_packet_size
                .and_then(|size| usize::try_from(size).ok())
                .unwrap_or(usize::MAX),
            auth_handler: self.auth_handler.clone(),
            auth_method: self.auth_method.clone(),
            keepalive_state: keepalive_state.clone(),
            codec_registry: self.options.codec_registry.clone(),
            deferred_ack: self.options.deferred_ack,
            ack_callbacks: Arc::clone(&self.ack_callbacks),
            ack_dispatcher: Arc::clone(&self.ack_dispatcher),
            topic_aliases: Arc::new(Mutex::new(crate::session::TopicAliasManager::new(
                self.options.properties.topic_alias_maximum.unwrap_or(0),
            ))),
            request_problem_information: self
                .options
                .properties
                .request_problem_information
                .unwrap_or(true),
        };

        let ctx_for_packet_reader = ctx.clone();
        let packet_reader = tokio::spawn(async move {
            tracing::debug!("📦 PACKET READER - Task starting");
            packet_reader_task_with_responses(reader, ctx_for_packet_reader).await;
            tracing::debug!("📦 PACKET READER - Task exited");
        });
        *reader_task.lock() = Some(packet_reader.abort_handle());
        self.packet_reader_handle = Some(packet_reader);

        let keepalive_interval = self.negotiated_keep_alive();
        if keepalive_interval.is_zero() {
            tracing::debug!("💓 KEEPALIVE - Disabled (interval is zero)");
        } else {
            let keepalive_writer = writer_for_keepalive;
            let keepalive_config = self.options.keepalive_config;
            self.keepalive_handle = Some(tokio::spawn(async move {
                tracing::debug!("💓 KEEPALIVE - Task starting");
                keepalive_task_with_writer(
                    keepalive_writer,
                    keepalive_interval,
                    keepalive_state,
                    lifecycle,
                    keepalive_config,
                )
                .await;
                tracing::debug!("💓 KEEPALIVE - Task exited");
            }));
        }

        #[cfg(feature = "transport-quic")]
        if let Some(conn) = &self.quic_connection {
            let connection = conn.clone();
            let ctx_for_streams = ctx.clone();
            self.quic_stream_acceptor_handle = Some(tokio::spawn(async move {
                tracing::debug!("🔀 QUIC STREAM ACCEPTOR - Task starting");
                quic_stream_acceptor_task(connection, ctx_for_streams).await;
                tracing::debug!("🔀 QUIC STREAM ACCEPTOR - Task exited");
            }));
            tracing::debug!("🔀 QUIC STREAM ACCEPTOR - Started (always runs to accept server-initiated streams)");

            let session_for_expiration = self.session.clone();
            self.flow_expiration_handle = Some(tokio::spawn(async move {
                tracing::debug!("⏰ FLOW EXPIRATION - Task starting");
                flow_expiration_task(session_for_expiration).await;
                tracing::debug!("⏰ FLOW EXPIRATION - Task exited");
            }));
            tracing::debug!("⏰ FLOW EXPIRATION - Started");
        }

        Ok(())
    }

    #[cfg(feature = "transport-quic")]
    async fn get_recoverable_flows(&self) -> Vec<(FlowId, FlowFlags)> {
        self.session.read().await.get_recoverable_flows().await
    }

    #[cfg(feature = "transport-quic")]
    pub(crate) async fn recover_flows(&self) -> Result<usize> {
        let Some(manager) = &self.quic_stream_manager else {
            return Ok(0);
        };

        let flows = self.get_recoverable_flows().await;
        let mut recovered = 0;

        for (flow_id, flags) in flows {
            let recovery_flags = FlowFlags { clean: 0, ..flags };

            match manager.open_recovery_stream(flow_id, recovery_flags).await {
                Ok(send) => {
                    manager.register_flow_stream(flow_id, send).await;
                    tracing::debug!(
                        flow_id = ?flow_id,
                        "Opened and registered recovery stream for flow"
                    );
                    recovered += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        flow_id = ?flow_id,
                        error = %e,
                        "Failed to open recovery stream"
                    );
                }
            }
        }

        tracing::info!(recovered = recovered, "Flow recovery completed");

        Ok(recovered)
    }

    #[cfg(feature = "transport-quic")]
    pub async fn discard_flow(&self, flow_id: FlowId) -> Result<()> {
        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }
        let manager = self.quic_stream_manager.as_ref().ok_or_else(|| {
            MqttError::ConnectionError("discard_flow only supported for QUIC connections".into())
        })?;
        manager.discard_flow(flow_id).await
    }

    #[cfg(feature = "transport-quic")]
    pub fn migrate(&self) -> Result<()> {
        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }
        let endpoint = self.quic_endpoint.as_ref().ok_or_else(|| {
            MqttError::ConnectionError("migration only supported for QUIC connections".into())
        })?;
        let socket = std::net::UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| MqttError::ConnectionError(format!("failed to bind new socket: {e}")))?;
        endpoint
            .rebind(socket)
            .map_err(|e| MqttError::ConnectionError(format!("failed to rebind endpoint: {e}")))?;
        tracing::info!(
            local_addr = ?endpoint.local_addr(),
            "QUIC endpoint rebound to new socket"
        );
        Ok(())
    }

    async fn stop_background_tasks(&mut self) {
        if let Some(handle) = self.packet_reader_handle.take() {
            handle.abort();
            let _ = handle.await;
        }
        if let Some(handle) = self.keepalive_handle.take() {
            handle.abort();
            let _ = handle.await;
        }
        #[cfg(feature = "transport-quic")]
        if let Some(handle) = self.quic_stream_acceptor_handle.take() {
            handle.abort();
            let _ = handle.await;
        }
        #[cfg(feature = "transport-quic")]
        if let Some(handle) = self.flow_expiration_handle.take() {
            handle.abort();
            let _ = handle.await;
        }
    }
}

fn maybe_store_subscriptions(
    stored_subscriptions: &StoredSubscriptions,
    filters: &[TopicFilter],
    subscription_identifier: Option<u32>,
    callback_id: CallbackId,
    persistence: SubscriptionPersistence,
) {
    if persistence == SubscriptionPersistence::Skip {
        return;
    }

    let mut stored = stored_subscriptions.lock();
    for filter in filters {
        stored.push((
            filter.filter.clone(),
            filter.options,
            subscription_identifier,
            callback_id,
        ));
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::packet::connack::ConnAckPacket;
    use crate::protocol::v5::reason_codes::ReasonCode;
    use crate::test_utils::*;
    use crate::transport::mock::MockTransport;

    fn create_test_client() -> DirectClientInner {
        let options = ConnectOptions::new("test-client")
            .with_clean_start(true)
            .with_keep_alive(Duration::from_secs(60));
        DirectClientInner::new(options)
    }

    #[tokio::test]
    pub async fn test_client_creation() {
        let client = create_test_client();
        assert!(!client.is_connected());
        assert!(client.writer.is_none());
        assert!(client.packet_reader_handle.is_none());
        assert!(client.keepalive_handle.is_none());
    }

    #[tokio::test]
    async fn test_connect_success() {
        let client = create_test_client();
        let transport = MockTransport::new();

        let connack = ConnAckPacket {
            protocol_version: 5,
            session_present: false,
            reason_code: ReasonCode::Success,
            properties: Properties::default(),
        };
        let connack_bytes = encode_packet(&Packet::ConnAck(connack)).unwrap();
        transport.inject_packet(connack_bytes).await;

        let transport_type = TransportType::Tcp(crate::transport::tcp::TcpTransport::from_addr(
            std::net::SocketAddr::from(([127, 0, 0, 1], 1883)),
        ));

        let mock_transport = MockTransport::new();
        mock_transport
            .inject_packet(
                encode_packet(&Packet::ConnAck(ConnAckPacket {
                    protocol_version: 5,
                    session_present: false,
                    reason_code: ReasonCode::Success,
                    properties: Properties::default(),
                }))
                .unwrap(),
            )
            .await;

        let _ = transport_type;
        assert!(!client.is_connected());

        let connect_packet = client.build_connect_packet().await;
        assert_eq!(connect_packet.client_id, "test-client");
        assert_eq!(connect_packet.keep_alive, 60);
        assert!(connect_packet.clean_start);
    }

    #[tokio::test]
    async fn test_publish_not_connected() {
        let client = create_test_client();

        let result = client
            .stage_publish(
                "test/topic".to_string(),
                b"test payload".to_vec(),
                PublishOptions::default(),
            )
            .await;

        assert!(matches!(result, Err(MqttError::NotConnected)));
    }

    #[tokio::test]
    async fn test_check_publish_size_enforces_negotiated_limit() {
        let client = create_test_client();
        client
            .session
            .write()
            .await
            .set_server_maximum_packet_size(1024)
            .await;

        let template = PublishPacket {
            topic_name: "test/flush".to_string(),
            payload: Vec::new().into(),
            qos: QoS::AtLeastOnce,
            retain: false,
            dup: true,
            packet_id: Some(SIZE_PROBE_PACKET_ID),
            properties: Properties::default(),
            protocol_version: 5,
            stream_id: None,
        };

        let within_limit = PublishPacket {
            payload: vec![0u8; 64].into(),
            ..template.clone()
        };
        assert!(client.check_publish_size(&within_limit).await.is_ok());

        let oversized = PublishPacket {
            payload: vec![0u8; 4096].into(),
            ..template
        };
        assert!(matches!(
            client.check_publish_size(&oversized).await,
            Err(MqttError::PacketTooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn test_oversized_publish_rejected_at_enqueue_while_disconnected() {
        let mut client = create_test_client();
        client.set_queue_on_disconnect(true);
        client
            .session
            .write()
            .await
            .set_server_maximum_packet_size(1024)
            .await;
        assert!(!client.is_connected());

        let oversized = client
            .stage_publish(
                "test/flush".to_string(),
                vec![0u8; 4096],
                PublishOptions {
                    qos: QoS::AtLeastOnce,
                    ..Default::default()
                },
            )
            .await;
        assert!(
            matches!(oversized, Err(MqttError::PacketTooLarge { .. })),
            "oversized publish must be rejected at enqueue time, not silently queued: {oversized:?}"
        );
        assert!(
            client.queued_messages.lock().is_empty(),
            "a rejected publish must not be queued"
        );

        let within = client
            .stage_publish(
                "test/flush".to_string(),
                vec![0u8; 64],
                PublishOptions {
                    qos: QoS::AtLeastOnce,
                    ..Default::default()
                },
            )
            .await;
        let Ok(StagedPublish::Queued(handle)) = within else {
            panic!("within-limit publish must queue: {within:?}");
        };
        assert!(
            !client.queued_messages.lock().is_empty(),
            "within-limit publish must be queued"
        );
        assert_eq!(
            handle.try_outcome(),
            None,
            "a queued publish must not report an outcome before it is flushed"
        );
    }

    #[tokio::test]
    async fn test_subscribe_not_connected() {
        let client = create_test_client();

        let packet = SubscribePacket {
            packet_id: 0,
            properties: Properties::default(),
            filters: vec![crate::packet::subscribe::TopicFilter {
                filter: "test/+".to_string(),
                options: SubscriptionOptions {
                    qos: QoS::AtLeastOnce,
                    no_local: false,
                    retain_as_published: false,
                    retain_handling: crate::packet::subscribe::RetainHandling::SendAtSubscribe,
                },
            }],
            protocol_version: 5,
        };

        let result = client.subscribe_with_callback(packet, 0).await;
        assert!(matches!(result, Err(MqttError::NotConnected)));
    }

    #[test]
    fn test_subscribe_internal_can_skip_persisting_stored_subscriptions() {
        let stored = Arc::new(Mutex::new(Vec::new()));
        let filters = vec![TopicFilter {
            filter: "test/topic".to_string(),
            options: SubscriptionOptions {
                qos: QoS::AtLeastOnce,
                no_local: false,
                retain_as_published: false,
                retain_handling: crate::packet::subscribe::RetainHandling::SendAtSubscribe,
            },
        }];

        maybe_store_subscriptions(&stored, &filters, None, 7, SubscriptionPersistence::Skip);
        assert!(stored.lock().is_empty());

        maybe_store_subscriptions(&stored, &filters, None, 7, SubscriptionPersistence::Persist);
        let stored = stored.lock();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].0, "test/topic");
        assert_eq!(stored[0].3, 7);
    }

    #[tokio::test]
    async fn test_unsubscribe_not_connected() {
        let client = create_test_client();

        let packet = UnsubscribePacket {
            packet_id: 0,
            properties: Properties::default(),
            filters: vec!["test/+".to_string()],
            protocol_version: 5,
        };

        let result = client.unsubscribe(packet).await;
        assert!(matches!(result, Err(MqttError::NotConnected)));
    }

    #[tokio::test]
    async fn test_disconnect_not_connected() {
        let mut client = create_test_client();
        let result = client.disconnect().await;
        assert!(matches!(result, Err(MqttError::NotConnected)));
    }

    #[tokio::test]
    async fn test_packet_id_generation() {
        let client = create_test_client();

        let id1 = client.packet_id_generator.next();
        let id2 = client.packet_id_generator.next();
        let id3 = client.packet_id_generator.next();

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[tokio::test]
    async fn test_connect_packet_with_will() {
        let will = crate::types::WillMessage::new("test/will", b"offline")
            .with_qos(QoS::AtLeastOnce)
            .with_retain(true);

        let options = ConnectOptions::new("test-client")
            .with_clean_start(true)
            .with_keep_alive(Duration::from_secs(60))
            .with_will(will);

        let client = DirectClientInner::new(options);
        let connect_packet = client.build_connect_packet().await;

        assert!(connect_packet.will.is_some());
        let will = connect_packet.will.unwrap();
        assert_eq!(will.topic, "test/will");
        assert_eq!(&will.payload[..], b"offline");
        assert_eq!(will.qos, QoS::AtLeastOnce);
        assert!(will.retain);
    }

    #[tokio::test]
    async fn test_connect_packet_with_auth() {
        let options = ConnectOptions::new("test-client")
            .with_clean_start(true)
            .with_keep_alive(Duration::from_secs(60))
            .with_credentials("user123", b"pass123");

        let client = DirectClientInner::new(options);
        let connect_packet = client.build_connect_packet().await;

        assert_eq!(connect_packet.username, Some("user123".to_string()));
        assert_eq!(connect_packet.password, Some(b"pass123".to_vec()));
    }

    #[tokio::test]
    async fn test_session_state_sharing() {
        let client = create_test_client();

        let session = client.session.read().await;
        assert_eq!(session.client_id(), "test-client");
        drop(session);

        let session = client.session.write().await;
        assert_eq!(session.client_id(), "test-client");
    }

    type SeenFrames = Arc<Mutex<Vec<(usize, u8)>>>;

    async fn read_frame(stream: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
        use tokio::io::AsyncReadExt;
        let mut first = [0u8; 1];
        stream.read_exact(&mut first).await.ok()?;
        let mut len = 0usize;
        let mut shift = 0;
        loop {
            let mut b = [0u8; 1];
            stream.read_exact(&mut b).await.ok()?;
            len |= usize::from(b[0] & 0x7f) << shift;
            shift += 7;
            if b[0] & 0x80 == 0 {
                break;
            }
        }
        let mut body = vec![0u8; len];
        stream.read_exact(&mut body).await.ok()?;
        let mut out = vec![first[0]];
        out.extend(body);
        Some(out)
    }

    async fn silent_broker(connacks: Vec<Vec<u8>>) -> (std::net::SocketAddr, SeenFrames) {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: SeenFrames = Arc::new(Mutex::new(Vec::new()));
        let seen_broker = Arc::clone(&seen);
        tokio::spawn(async move {
            for (conn, connack) in connacks.into_iter().enumerate() {
                let (mut s, _) = listener.accept().await.unwrap();
                read_frame(&mut s).await.unwrap();
                s.write_all(&connack).await.unwrap();
                let seen_conn = Arc::clone(&seen_broker);
                tokio::spawn(async move {
                    while let Some(f) = read_frame(&mut s).await {
                        seen_conn.lock().push((conn, f[0]));
                    }
                });
            }
        });
        (addr, seen)
    }

    async fn connect_to(client: &mut DirectClientInner, addr: std::net::SocketAddr) -> bool {
        use mqtt5_protocol::Transport;
        let mut transport = crate::transport::tcp::TcpTransport::from_addr(addr);
        transport.connect().await.unwrap();
        client
            .connect(TransportType::Tcp(transport))
            .await
            .unwrap()
            .session_present
    }

    fn publishes_on(seen: &SeenFrames, conn: usize) -> Vec<u8> {
        seen.lock()
            .iter()
            .filter(|(c, first)| *c == conn && first >> 4 == 3)
            .map(|(_, first)| *first)
            .collect()
    }

    fn qos(level: QoS) -> PublishOptions {
        PublishOptions {
            qos: level,
            ..Default::default()
        }
    }

    async fn claim_quota(client: &DirectClientInner, ready: &ReadyPublish) -> Option<u64> {
        let flow = Arc::clone(client.session.read().await.flow_control());
        match ready.packet_id() {
            Some(packet_id) => Some(
                FlowControlManager::acquire_shared_send_quota(&flow, packet_id)
                    .await
                    .unwrap(),
            ),
            None => None,
        }
    }

    async fn send_with_claim(
        client: &DirectClientInner,
        mut ready: Box<ReadyPublish>,
        mut claim: Option<u64>,
    ) -> usize {
        let mut restages = 0;
        loop {
            match client.transmit_publish(ready, claim).await.unwrap() {
                Transmitted::Sent(_) | Transmitted::InFlight(_) | Transmitted::Detached(_) => {
                    return restages;
                }
                Transmitted::Restaged(next) => {
                    restages += 1;
                    claim = claim_quota(client, &next).await;
                    ready = next;
                }
            }
        }
    }

    #[tokio::test]
    async fn stale_quota_claim_does_not_exceed_receive_maximum_after_reconnect() {
        let receive_maximum_one =
            |session_present: u8| vec![0x20, 0x06, session_present, 0x00, 0x03, 0x21, 0x00, 0x01];
        let (addr, seen) =
            silent_broker(vec![receive_maximum_one(0), receive_maximum_one(1)]).await;
        let mut client =
            DirectClientInner::new(ConnectOptions::new("stale-quota").with_clean_start(false));
        connect_to(&mut client, addr).await;

        let Ok(StagedPublish::Ready(b)) = client
            .stage_publish("t/b".into(), b"b".to_vec(), qos(QoS::AtLeastOnce))
            .await
        else {
            panic!("publish b must stage while connected");
        };
        let flow = Arc::clone(client.session.read().await.flow_control());
        let stale_claim = claim_quota(&client, &b).await;

        assert!(connect_to(&mut client, addr).await);

        assert_eq!(send_with_claim(&client, b, stale_claim).await, 1);
        assert_eq!(flow.read().await.in_flight_count().await, 1);

        let Ok(StagedPublish::Ready(c)) = client
            .stage_publish("t/c".into(), b"c".to_vec(), qos(QoS::AtLeastOnce))
            .await
        else {
            panic!("publish c must stage while connected");
        };
        let c_quota = tokio::time::timeout(
            Duration::from_millis(500),
            FlowControlManager::acquire_shared_send_quota(&flow, c.packet_id().unwrap()),
        )
        .await;
        if let Ok(Ok(generation)) = c_quota {
            send_with_claim(&client, c, Some(generation)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        let conn2 = publishes_on(&seen, 1);
        assert!(
            conn2.len() <= 1,
            "client exceeded server Receive Maximum 1 on the new connection: {conn2:x?}"
        );
    }

    #[tokio::test]
    async fn publish_waiting_across_reconnect_conforms_to_new_connection() {
        let first = vec![0x20, 0x06, 0x00, 0x00, 0x03, 0x21, 0x00, 0x01];
        let maximum_qos_one = vec![0x20, 0x05, 0x01, 0x00, 0x02, 0x24, 0x01];
        let (addr, seen) = silent_broker(vec![first, maximum_qos_one]).await;
        let mut client =
            DirectClientInner::new(ConnectOptions::new("reconform").with_clean_start(false));
        connect_to(&mut client, addr).await;
        let flow = Arc::clone(client.session.read().await.flow_control());

        let Ok(StagedPublish::Ready(a)) = client
            .stage_publish("t/a".into(), b"a".to_vec(), qos(QoS::AtLeastOnce))
            .await
        else {
            panic!("publish a must stage while connected");
        };
        let a_claim = claim_quota(&client, &a).await;
        assert_eq!(send_with_claim(&client, a, a_claim).await, 0);

        let Ok(StagedPublish::Ready(b)) = client
            .stage_publish("t/b".into(), b"b".to_vec(), qos(QoS::ExactlyOnce))
            .await
        else {
            panic!("publish b must stage while connected");
        };
        let b_id = b.packet_id().unwrap();
        let waiting_flow = Arc::clone(&flow);
        let waiting = tokio::spawn(async move {
            FlowControlManager::acquire_shared_send_quota(&waiting_flow, b_id).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "b must wait for Receive Maximum 1");

        assert!(connect_to(&mut client, addr).await);
        let b_claim = waiting.await.unwrap().unwrap();
        assert_eq!(send_with_claim(&client, b, Some(b_claim)).await, 1);

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            publishes_on(&seen, 1),
            vec![0x3A, 0x32],
            "the replayed QoS 1 publish goes first, then b downgraded to the new Maximum QoS 1"
        );
    }

    async fn unacked_packet_ids(client: &DirectClientInner) -> Vec<u16> {
        client
            .session
            .read()
            .await
            .get_unacked_publishes()
            .await
            .iter()
            .filter_map(|publish| publish.packet_id)
            .collect()
    }

    #[tokio::test]
    async fn stale_flush_task_does_not_write_after_reconnect() {
        let resume_receive_maximum_one = vec![0x20, 0x06, 0x01, 0x00, 0x03, 0x21, 0x00, 0x01];
        let resume = vec![0x20, 0x03, 0x01, 0x00, 0x00];
        let (addr, seen) = silent_broker(vec![
            vec![0x20, 0x03, 0x00, 0x00, 0x00],
            resume_receive_maximum_one,
            resume,
        ])
        .await;
        let mut client =
            DirectClientInner::new(ConnectOptions::new("stale-flush").with_clean_start(false));
        connect_to(&mut client, addr).await;
        client.set_connected(false);
        for topic in ["t/a", "t/b"] {
            let queued = client
                .stage_publish(topic.into(), b"x".to_vec(), qos(QoS::AtLeastOnce))
                .await;
            assert!(matches!(queued, Ok(StagedPublish::Queued(_))));
        }

        assert!(connect_to(&mut client, addr).await);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            publishes_on(&seen, 1).len(),
            1,
            "setup: a flushed, b waits for quota"
        );
        let stale_writer = Arc::clone(client.writer.as_ref().unwrap());
        let held = stale_writer.lock().await;
        let flow = Arc::clone(client.session.read().await.flow_control());
        let a = unacked_packet_ids(&client).await[0];
        flow.read().await.acknowledge(a).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(connect_to(&mut client, addr).await);
        drop(held);
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(
            publishes_on(&seen, 1).len(),
            1,
            "the flush task of the replaced connection wrote after the reconnect"
        );
    }

    #[tokio::test]
    async fn abandoned_qos2_replay_id_is_quarantined_until_session_is_lost() {
        let first = vec![0x20, 0x03, 0x00, 0x00, 0x00];
        let resume = vec![0x20, 0x03, 0x01, 0x00, 0x00];
        let resume_maximum_qos_one = vec![0x20, 0x05, 0x01, 0x00, 0x02, 0x24, 0x01];
        let lost = vec![0x20, 0x03, 0x00, 0x00, 0x00];
        let (addr, seen) = silent_broker(vec![first, resume, resume_maximum_qos_one, lost]).await;
        let mut client =
            DirectClientInner::new(ConnectOptions::new("quarantine").with_clean_start(false));
        connect_to(&mut client, addr).await;
        client.set_connected(false);
        let queued = client
            .stage_publish("t/two".into(), b"x".to_vec(), qos(QoS::ExactlyOnce))
            .await;
        assert!(matches!(queued, Ok(StagedPublish::Queued(_))));

        assert!(connect_to(&mut client, addr).await);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let abandoned = unacked_packet_ids(&client).await[0];
        assert_eq!(publishes_on(&seen, 1), vec![0x34]);

        assert!(connect_to(&mut client, addr).await);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            publishes_on(&seen, 2).is_empty(),
            "QoS 2 replay after Maximum QoS 1"
        );
        assert!(unacked_packet_ids(&client).await.is_empty());

        let mut handed_out = false;
        for _ in 0..u16::MAX {
            let reservation = client.allocate_packet_id().await.unwrap();
            handed_out |= reservation.packet_id() == abandoned;
        }
        assert!(
            !handed_out,
            "a quarantined QoS 2 identifier was reallocated"
        );

        assert!(!connect_to(&mut client, addr).await);
        let mut released = false;
        for _ in 0..u16::MAX {
            let reservation = client.allocate_packet_id().await.unwrap();
            released |= reservation.packet_id() == abandoned;
        }
        assert!(released, "Session Present 0 must release the quarantine");
    }

    #[derive(Clone, Copy)]
    enum AckScript {
        Acknowledged,
        ReceiptRefused,
        Completed,
    }

    async fn ack_processed_while_disconnecting(script: AckScript) -> Option<PublishOutcome> {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (pid_tx, mut pid_rx) = tokio::sync::mpsc::unbounded_channel::<u16>();
        let (ack_tx, ack_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let (mut s0, _) = listener.accept().await.unwrap();
            read_frame(&mut s0).await.unwrap();
            s0.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).await.unwrap();
            tokio::spawn(async move { while read_frame(&mut s0).await.is_some() {} });
            let (mut s1, _) = listener.accept().await.unwrap();
            read_frame(&mut s1).await.unwrap();
            s1.write_all(&[0x20, 0x03, 0x01, 0x00, 0x00]).await.unwrap();
            let pid = loop {
                let f = read_frame(&mut s1).await.unwrap();
                if f[0] >> 4 == 3 {
                    let tlen = usize::from(u16::from_be_bytes([f[1], f[2]]));
                    break u16::from_be_bytes([f[3 + tlen], f[4 + tlen]]);
                }
            };
            let [hi, lo] = pid.to_be_bytes();
            if matches!(script, AckScript::Completed) {
                s1.write_all(&[0x50, 0x02, hi, lo]).await.unwrap();
                while read_frame(&mut s1).await.unwrap()[0] != 0x62 {}
            }
            pid_tx.send(pid).unwrap();
            ack_rx.await.unwrap();
            let ack: &[u8] = match script {
                AckScript::Acknowledged => &[0x40, 0x02, hi, lo],
                AckScript::ReceiptRefused => &[0x50, 0x03, hi, lo, 0x80],
                AckScript::Completed => &[0x70, 0x02, hi, lo],
            };
            s1.write_all(ack).await.unwrap();
            tokio::spawn(async move { while read_frame(&mut s1).await.is_some() {} });
            let (mut s2, _) = listener.accept().await.unwrap();
            read_frame(&mut s2).await.unwrap();
            s2.write_all(&[0x20, 0x03, 0x01, 0x00, 0x00]).await.unwrap();
            while read_frame(&mut s2).await.is_some() {}
        });

        let level = match script {
            AckScript::Acknowledged => QoS::AtLeastOnce,
            AckScript::ReceiptRefused | AckScript::Completed => QoS::ExactlyOnce,
        };
        let mut client =
            DirectClientInner::new(ConnectOptions::new("ack-abort").with_clean_start(false));
        connect_to(&mut client, addr).await;
        client.set_connected(false);
        let Ok(StagedPublish::Queued(handle)) = client
            .stage_publish("t/a".into(), b"a".to_vec(), qos(level))
            .await
        else {
            panic!("setup: offline publish must queue");
        };
        assert!(connect_to(&mut client, addr).await);
        pid_rx.recv().await.unwrap();

        let flow = Arc::clone(client.session.read().await.flow_control());
        let guard = Arc::clone(&flow).write_owned().await;
        ack_tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            drop(guard);
        });
        client.disconnect().await.unwrap();

        assert!(connect_to(&mut client, addr).await);
        tokio::time::sleep(Duration::from_millis(300)).await;
        handle.try_outcome()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn puback_processed_while_disconnecting_settles_the_handle() {
        assert!(matches!(
            ack_processed_while_disconnecting(AckScript::Acknowledged).await,
            Some(PublishOutcome::Delivered(Delivery::AtLeastOnce { .. }))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn refused_pubrec_processed_while_disconnecting_settles_the_handle() {
        assert_eq!(
            ack_processed_while_disconnecting(AckScript::ReceiptRefused).await,
            Some(PublishOutcome::Rejected(PublishRejection::Refused(
                ReasonCode::UnspecifiedError
            )))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pubcomp_processed_while_disconnecting_settles_the_handle() {
        assert!(matches!(
            ack_processed_while_disconnecting(AckScript::Completed).await,
            Some(PublishOutcome::Delivered(Delivery::ExactlyOnce { .. }))
        ));
    }

    #[tokio::test]
    async fn publish_staged_before_reconnect_is_restaged_even_with_a_current_quota_claim() {
        let first = vec![0x20, 0x03, 0x00, 0x00, 0x00];
        let maximum_qos_one = vec![0x20, 0x05, 0x01, 0x00, 0x02, 0x24, 0x01];
        let (addr, seen) = silent_broker(vec![first, maximum_qos_one]).await;
        let mut client =
            DirectClientInner::new(ConnectOptions::new("epoch-only").with_clean_start(false));
        connect_to(&mut client, addr).await;
        let Ok(StagedPublish::Ready(staged)) = client
            .stage_publish("t/e".into(), b"e".to_vec(), qos(QoS::ExactlyOnce))
            .await
        else {
            panic!("publish must stage while connected");
        };

        assert!(connect_to(&mut client, addr).await);
        let current_claim = claim_quota(&client, &staged).await;
        assert_eq!(send_with_claim(&client, staged, current_claim).await, 1);

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            publishes_on(&seen, 1),
            vec![0x32],
            "a publish staged on the previous connection must be conformed to the new Maximum QoS 1"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn abandoned_qos2_id_is_quarantined_before_it_leaves_the_session_store() {
        let first = vec![0x20, 0x03, 0x00, 0x00, 0x00];
        let resume = vec![0x20, 0x03, 0x01, 0x00, 0x00];
        let resume_maximum_qos_one = vec![0x20, 0x05, 0x01, 0x00, 0x02, 0x24, 0x01];
        let (addr, _seen) = silent_broker(vec![first, resume, resume_maximum_qos_one]).await;
        let mut client =
            DirectClientInner::new(ConnectOptions::new("quarantine-order").with_clean_start(false));
        connect_to(&mut client, addr).await;
        client.set_connected(false);
        let queued = client
            .stage_publish("t/two".into(), b"x".to_vec(), qos(QoS::ExactlyOnce))
            .await;
        assert!(matches!(queued, Ok(StagedPublish::Queued(_))));
        assert!(connect_to(&mut client, addr).await);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let abandoned = unacked_packet_ids(&client).await[0];

        let ids = Arc::clone(&client.outbound_ids);
        let (locked_tx, locked_rx) = oneshot::channel::<()>();
        let (inspect_tx, inspect_rx) = std::sync::mpsc::channel::<()>();
        let (state_tx, state_rx) = oneshot::channel::<bool>();
        let holder = std::thread::spawn(move || {
            let held = ids.lock();
            let _ = locked_tx.send(());
            let _ = inspect_rx.recv();
            let _ = state_tx.send(held.holds(abandoned));
        });
        locked_rx.await.unwrap();
        assert!(connect_to(&mut client, addr).await);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let in_store = unacked_packet_ids(&client).await.contains(&abandoned);
        inspect_tx.send(()).unwrap();
        let quarantined = state_rx.await.unwrap();
        holder.join().unwrap();

        assert!(
            in_store || quarantined,
            "packet id {abandoned} left the session store before it was quarantined; an allocation in that window could reuse it"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(client.outbound_ids.lock().holds(abandoned));
    }

    #[tokio::test]
    async fn downgraded_message_is_requeued_when_the_connection_ends_before_it_is_written() {
        let first = vec![0x20, 0x03, 0x00, 0x00, 0x00];
        let resume_maximum_qos_zero = vec![0x20, 0x05, 0x01, 0x00, 0x02, 0x24, 0x00];
        let resume = vec![0x20, 0x03, 0x01, 0x00, 0x00];
        let (addr, seen) = silent_broker(vec![first, resume_maximum_qos_zero, resume]).await;
        let mut client = DirectClientInner::new(
            ConnectOptions::new("downgrade-requeue").with_clean_start(false),
        );
        connect_to(&mut client, addr).await;
        client.set_connected(false);
        let Ok(StagedPublish::Queued(handle)) = client
            .stage_publish("t/one".into(), b"x".to_vec(), qos(QoS::AtLeastOnce))
            .await
        else {
            panic!("setup: offline publish must queue");
        };

        assert!(connect_to(&mut client, addr).await);
        let replaced_writer = Arc::clone(client.writer.as_ref().unwrap());
        let held = replaced_writer.lock().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(connect_to(&mut client, addr).await);
        drop(held);
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(
            publishes_on(&seen, 1).is_empty(),
            "nothing reaches the replaced connection"
        );
        assert_eq!(
            publishes_on(&seen, 2),
            vec![0x32],
            "the message that never reached the wire is re-queued and sent on the next connection"
        );
        assert_eq!(handle.try_outcome(), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stuck_replay_on_replaced_connection_does_not_starve_the_new_one() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: SeenFrames = Arc::new(Mutex::new(Vec::new()));
        let seen_broker = Arc::clone(&seen);
        let (hold_tx, mut hold_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let connacks = [
                vec![0x20, 0x03, 0x00, 0x00, 0x00],
                vec![0x20, 0x03, 0x01, 0x00, 0x00],
                vec![0x20, 0x03, 0x01, 0x00, 0x00],
            ];
            for (conn, connack) in connacks.into_iter().enumerate() {
                let (mut s, _) = listener.accept().await.unwrap();
                read_frame(&mut s).await.unwrap();
                s.write_all(&connack).await.unwrap();
                if conn == 1 {
                    let _ = hold_tx.send(s);
                    continue;
                }
                let seen_conn = Arc::clone(&seen_broker);
                tokio::spawn(async move {
                    while let Some(f) = read_frame(&mut s).await {
                        seen_conn.lock().push((conn, f[0]));
                    }
                });
            }
        });
        let mut client =
            DirectClientInner::new(ConnectOptions::new("stuck-replay").with_clean_start(false));
        connect_to(&mut client, addr).await;
        client.set_connected(false);
        for i in 0..64 {
            let queued = client
                .stage_publish(
                    format!("t/{i}"),
                    vec![0u8; 256 * 1024],
                    qos(QoS::AtLeastOnce),
                )
                .await;
            assert!(matches!(queued, Ok(StagedPublish::Queued(_))));
        }
        assert!(connect_to(&mut client, addr).await);
        let _held = hold_rx.recv().await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;

        assert!(connect_to(&mut client, addr).await);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let on_new = publishes_on(&seen, 2).len();
        let Ok(StagedPublish::Ready(live)) = client
            .stage_publish("t/live".into(), b"x".to_vec(), qos(QoS::AtLeastOnce))
            .await
        else {
            panic!("live publish must stage");
        };
        let flow = Arc::clone(client.session.read().await.flow_control());
        let quota = tokio::time::timeout(
            Duration::from_secs(3),
            FlowControlManager::acquire_shared_send_quota(&flow, live.packet_id().unwrap()),
        )
        .await;
        assert!(
            on_new > 0 && quota.is_ok(),
            "new healthy connection is starved by a replay stuck on the replaced connection: resent on new={on_new}, live quota={quota:?}"
        );
    }

    #[tokio::test]
    async fn stored_publish_whose_write_fails_is_returned_detached() {
        let (addr, _seen) = silent_broker(vec![vec![0x20, 0x03, 0x00, 0x00, 0x00]]).await;
        let mut client =
            DirectClientInner::new(ConnectOptions::new("detached").with_clean_start(false));
        connect_to(&mut client, addr).await;
        let Ok(StagedPublish::Ready(ready)) = client
            .stage_publish("t/d".into(), b"d".to_vec(), qos(QoS::AtLeastOnce))
            .await
        else {
            panic!("publish must stage while connected");
        };
        let packet_id = ready.packet_id().unwrap();
        let claim = claim_quota(&client, &ready).await;
        client
            .writer
            .as_ref()
            .unwrap()
            .lock()
            .await
            .close(None)
            .await
            .unwrap();

        let transmitted = client.transmit_publish(ready, claim).await;
        let Ok(Transmitted::Detached(handle)) = transmitted else {
            panic!("a stored publish whose write failed must come back as a pending handle");
        };
        assert_eq!(handle.try_outcome(), None);
        assert_eq!(unacked_packet_ids(&client).await, vec![packet_id]);
    }

    #[tokio::test]
    async fn disconnect_while_already_disconnected_closes_send_quota() {
        let receive_maximum_one = vec![0x20, 0x06, 0x00, 0x00, 0x03, 0x21, 0x00, 0x01];
        let (addr, _seen) = silent_broker(vec![receive_maximum_one]).await;
        let mut client =
            DirectClientInner::new(ConnectOptions::new("disconnect-quota").with_clean_start(false));
        connect_to(&mut client, addr).await;
        let Ok(StagedPublish::Ready(first)) = client
            .stage_publish("t/1".into(), b"1".to_vec(), qos(QoS::AtLeastOnce))
            .await
        else {
            panic!("publish must stage while connected");
        };
        let claim = claim_quota(&client, &first).await;
        send_with_claim(&client, first, claim).await;
        let flow = Arc::clone(client.session.read().await.flow_control());
        let waiting = tokio::spawn(async move {
            FlowControlManager::acquire_shared_send_quota(&flow, 4242).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "setup: the send quota is exhausted");

        client.set_connected(false);
        assert!(matches!(
            client.disconnect().await,
            Err(MqttError::NotConnected)
        ));
        let waited = tokio::time::timeout(Duration::from_secs(2), waiting).await;
        assert!(
            matches!(waited, Ok(Ok(Err(MqttError::NotConnected)))),
            "disconnect must release publishes waiting for send quota: {waited:?}"
        );
    }

    async fn mismatched_acknowledgement(level: QoS, ack_type: u8) -> (Vec<u8>, bool) {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (frames_tx, frames_rx) = oneshot::channel::<Vec<Vec<u8>>>();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            read_frame(&mut s).await.unwrap();
            s.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).await.unwrap();
            let pid = loop {
                let f = read_frame(&mut s).await.unwrap();
                if f[0] >> 4 == 3 {
                    let tlen = usize::from(u16::from_be_bytes([f[1], f[2]]));
                    break u16::from_be_bytes([f[3 + tlen], f[4 + tlen]]);
                }
            };
            let [hi, lo] = pid.to_be_bytes();
            s.write_all(&[ack_type, 0x02, hi, lo]).await.unwrap();
            let mut after = Vec::new();
            while let Ok(Some(f)) =
                tokio::time::timeout(Duration::from_millis(500), read_frame(&mut s)).await
            {
                after.push(f);
            }
            let _ = frames_tx.send(after);
        });
        let mut client =
            DirectClientInner::new(ConnectOptions::new("ack-mismatch").with_clean_start(false));
        connect_to(&mut client, addr).await;
        let Ok(StagedPublish::Ready(ready)) = client
            .stage_publish("t/m".into(), b"m".to_vec(), qos(level))
            .await
        else {
            panic!("publish must stage while connected");
        };
        let packet_id = ready.packet_id().unwrap();
        let claim = claim_quota(&client, &ready).await;
        send_with_claim(&client, ready, claim).await;
        let after = frames_rx.await.unwrap();
        let disconnect = after
            .into_iter()
            .find(|f| f[0] == 0xE0)
            .map(|f| f[1..].to_vec())
            .unwrap_or_default();
        let still_held = unacked_packet_ids(&client).await.contains(&packet_id);
        (disconnect, still_held)
    }

    #[tokio::test]
    async fn puback_for_qos2_publish_is_a_protocol_error() {
        let (disconnect, still_held) = mismatched_acknowledgement(QoS::ExactlyOnce, 0x40).await;
        assert_eq!(disconnect.first(), Some(&0x82));
        assert!(
            still_held,
            "a mismatched PUBACK must not release the QoS 2 state"
        );
    }

    #[tokio::test]
    async fn pubcomp_for_qos1_publish_is_a_protocol_error() {
        let (disconnect, still_held) = mismatched_acknowledgement(QoS::AtLeastOnce, 0x70).await;
        assert_eq!(disconnect.first(), Some(&0x82));
        assert!(
            still_held,
            "a mismatched PUBCOMP must not release the QoS 1 state"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn refused_pubrec_aborted_before_removal_keeps_the_publish_unsettled() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (pid_tx, mut pid_rx) = tokio::sync::mpsc::unbounded_channel::<u16>();
        let (ack_tx, ack_rx) = oneshot::channel::<()>();
        let later: SeenFrames = Arc::new(Mutex::new(Vec::new()));
        let later_broker = Arc::clone(&later);
        tokio::spawn(async move {
            let (mut s1, _) = listener.accept().await.unwrap();
            read_frame(&mut s1).await.unwrap();
            s1.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).await.unwrap();
            let pid = loop {
                let f = read_frame(&mut s1).await.unwrap();
                if f[0] >> 4 == 3 {
                    let tlen = usize::from(u16::from_be_bytes([f[1], f[2]]));
                    break u16::from_be_bytes([f[3 + tlen], f[4 + tlen]]);
                }
            };
            let [hi, lo] = pid.to_be_bytes();
            pid_tx.send(pid).unwrap();
            ack_rx.await.unwrap();
            s1.write_all(&[0x50, 0x03, hi, lo, 0x80]).await.unwrap();
            tokio::spawn(async move { while read_frame(&mut s1).await.is_some() {} });
            let (mut s2, _) = listener.accept().await.unwrap();
            read_frame(&mut s2).await.unwrap();
            s2.write_all(&[0x20, 0x03, 0x01, 0x00, 0x00]).await.unwrap();
            while let Some(f) = read_frame(&mut s2).await {
                later_broker.lock().push((1, f[0]));
            }
        });
        let mut client =
            DirectClientInner::new(ConnectOptions::new("pubrec-abort").with_clean_start(false));
        connect_to(&mut client, addr).await;
        let Ok(StagedPublish::Ready(ready)) = client
            .stage_publish("t/a".into(), b"a".to_vec(), qos(QoS::ExactlyOnce))
            .await
        else {
            panic!("publish must stage");
        };
        let claim = claim_quota(&client, &ready).await;
        let Ok(Transmitted::InFlight(in_flight)) = client.transmit_publish(ready, claim).await
        else {
            panic!("live publish must be in flight");
        };
        let handle = in_flight.handle.clone();
        pid_rx.recv().await.unwrap();
        let session = Arc::clone(&client.session);
        let guard = session.read_owned().await;
        ack_tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let outcome_at_abort = handle.try_outcome();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            drop(guard);
        });
        client.disconnect().await.unwrap();
        assert!(connect_to(&mut client, addr).await);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let resent = publishes_on(&later, 1);
        assert_eq!(
            outcome_at_abort, None,
            "a PUBREC refusal that was not applied to the session must not settle the publish"
        );
        assert!(
            !resent.is_empty(),
            "the still-stored PUBLISH is replayed on resume"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stuck_downgraded_write_does_not_block_reconnect() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: SeenFrames = Arc::new(Mutex::new(Vec::new()));
        let seen_broker = Arc::clone(&seen);
        let (hold_tx, mut hold_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let connacks = [
                vec![0x20, 0x03, 0x00, 0x00, 0x00],
                vec![0x20, 0x05, 0x01, 0x00, 0x02, 0x24, 0x00],
                vec![0x20, 0x03, 0x01, 0x00, 0x00],
            ];
            for (conn, connack) in connacks.into_iter().enumerate() {
                let (mut s, _) = listener.accept().await.unwrap();
                read_frame(&mut s).await.unwrap();
                s.write_all(&connack).await.unwrap();
                if conn == 1 {
                    let _ = hold_tx.send(s);
                    continue;
                }
                let seen_conn = Arc::clone(&seen_broker);
                tokio::spawn(async move {
                    while let Some(f) = read_frame(&mut s).await {
                        seen_conn.lock().push((conn, f[0]));
                    }
                });
            }
        });
        let mut client =
            DirectClientInner::new(ConnectOptions::new("stuck-downgrade").with_clean_start(false));
        connect_to(&mut client, addr).await;
        client.set_connected(false);
        let mut handles = Vec::new();
        for i in 0..64 {
            let Ok(StagedPublish::Queued(h)) = client
                .stage_publish(
                    format!("t/{i}"),
                    vec![0u8; 256 * 1024],
                    qos(QoS::AtLeastOnce),
                )
                .await
            else {
                panic!("queue");
            };
            handles.push(h);
        }
        assert!(connect_to(&mut client, addr).await);
        let _held = hold_rx.recv().await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let reconnected =
            tokio::time::timeout(Duration::from_secs(3), connect_to(&mut client, addr)).await;
        assert!(
            reconnected.is_ok(),
            "reconnect blocked behind a stuck downgraded write"
        );
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let on_new = publishes_on(&seen, 2).len();
        assert!(
            on_new > 0,
            "remaining queued messages flushed on the new connection"
        );
    }

    #[tokio::test]
    async fn refused_pubrec_after_pubrel_is_a_protocol_error() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (frames_tx, frames_rx) = oneshot::channel::<Vec<Vec<u8>>>();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            read_frame(&mut s).await.unwrap();
            s.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).await.unwrap();
            let pid = loop {
                let f = read_frame(&mut s).await.unwrap();
                if f[0] >> 4 == 3 {
                    let tlen = usize::from(u16::from_be_bytes([f[1], f[2]]));
                    break u16::from_be_bytes([f[3 + tlen], f[4 + tlen]]);
                }
            };
            let [hi, lo] = pid.to_be_bytes();
            s.write_all(&[0x50, 0x02, hi, lo]).await.unwrap();
            while read_frame(&mut s).await.unwrap()[0] != 0x62 {}
            s.write_all(&[0x50, 0x03, hi, lo, 0x80]).await.unwrap();
            let mut after = Vec::new();
            while let Ok(Some(f)) =
                tokio::time::timeout(Duration::from_millis(500), read_frame(&mut s)).await
            {
                after.push(f);
            }
            let _ = frames_tx.send(after);
        });
        let mut client = DirectClientInner::new(
            ConnectOptions::new("pubrec-after-pubrel").with_clean_start(false),
        );
        connect_to(&mut client, addr).await;
        let Ok(StagedPublish::Ready(ready)) = client
            .stage_publish("t/r".into(), b"r".to_vec(), qos(QoS::ExactlyOnce))
            .await
        else {
            panic!("publish must stage while connected");
        };
        let packet_id = ready.packet_id().unwrap();
        let claim = claim_quota(&client, &ready).await;
        send_with_claim(&client, ready, claim).await;
        let after = frames_rx.await.unwrap();
        let disconnect = after.into_iter().find(|f| f[0] == 0xE0);
        assert_eq!(
            disconnect.and_then(|f| f.get(1).copied()),
            Some(0x82),
            "an error PUBREC for an identifier already released with PUBREL is a protocol error"
        );
        assert_eq!(
            client.session.read().await.outbound_stage(packet_id).await,
            Some(crate::session::state::OutboundStage::AwaitingPubComp),
            "the PUBREL state must be left untouched"
        );
    }

    #[tokio::test]
    async fn acknowledgement_settled_before_loss_is_reported_as_sent() {
        let (completion, handle) = Completion::new();
        completion.delivered(Delivery::AtLeastOnce { packet_id: 9 });
        let (alive, link) = watch::channel(false);
        drop(alive);
        let settled = InFlight { handle, link }.settle().await;
        assert!(
            matches!(
                settled,
                Ok(PublishResult::Sent(Delivery::AtLeastOnce { packet_id: 9 }))
            ),
            "an acknowledgement that settled before the connection ended must win: {settled:?}"
        );
    }

    #[tokio::test]
    async fn packet_id_allocation_does_not_scan_the_offline_queue() {
        let client = DirectClientInner::new(ConnectOptions::new("offline").with_clean_start(false));
        let mut held: Vec<IdReservation> = (1..u16::MAX)
            .filter_map(|packet_id| IdReservation::claim(&client.outbound_ids, packet_id))
            .collect();
        assert_eq!(held.len(), usize::from(u16::MAX - 1));

        let start = std::time::Instant::now();
        let last = client.allocate_packet_id().await;
        let exhausted = client.allocate_packet_id().await;
        let elapsed = start.elapsed();

        assert_eq!(
            last.as_ref().ok().map(IdReservation::packet_id),
            Some(u16::MAX)
        );
        assert!(matches!(exhausted, Err(MqttError::PacketIdExhausted)));
        assert!(
            elapsed < Duration::from_secs(1),
            "allocating against a full set of reserved ids took {elapsed:?}"
        );

        held.remove(0);
        assert_eq!(
            client
                .allocate_packet_id()
                .await
                .ok()
                .map(|r| r.packet_id()),
            Some(1)
        );
    }
}
