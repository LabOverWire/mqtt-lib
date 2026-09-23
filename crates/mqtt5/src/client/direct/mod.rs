//! Direct async client implementation
//!
//! This module implements the MQTT client using direct async calls.

pub(crate) mod ack;
mod handlers;
mod keepalive;
mod outbound;
mod reader;
mod replay;
mod unified;

pub use ack::AckToken;
pub(crate) use ack::{AckCallbackManager, AckDispatcher};

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Duration;

use crate::callback::{CallbackId, CallbackManager};
use crate::client::auth_handler::{AuthHandler, AuthResponse};
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
use crate::types::{ConnectOptions, ConnectResult, PublishOptions, PublishResult};
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
use replay::{PublishPolicy, SessionReplay};

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
pub(crate) type PendingAcks = Arc<Mutex<HashMap<u16, oneshot::Sender<ReasonCode>>>>;

#[derive(Debug)]
pub(crate) enum StagedPublish {
    Queued(PublishResult),
    Ready(PublishPacket),
}

pub(crate) struct PublishAck {
    rx: oneshot::Receiver<ReasonCode>,
    packet_id: u16,
    pending: PendingAcks,
}

impl PublishAck {
    pub(crate) async fn wait(self) -> Result<()> {
        match tokio::time::timeout(Duration::from_secs(10), self.rx).await {
            Ok(Ok(reason_code)) if reason_code.is_error() => {
                Err(MqttError::PublishFailed(reason_code))
            }
            Ok(Ok(_)) => Ok(()),
            Ok(Err(_)) => Err(MqttError::ProtocolError(
                "Acknowledgment channel closed".to_string(),
            )),
            Err(_) => {
                self.pending.lock().remove(&self.packet_id);
                Err(MqttError::Timeout)
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
    pub pending_pubacks: PendingAcks,
    pub pending_pubcomps: PendingAcks,
    pub reconnect_attempt: u32,
    pub last_address: Option<String>,
    pub automatic_reconnect_lifecycle: AutomaticReconnectLifecycle,
    pub server_redirect: Option<String>,
    pub queued_messages: Arc<Mutex<VecDeque<PublishPacket>>>,
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
        let session = Arc::new(tokio::sync::RwLock::new(SessionState::new(
            options.client_id.clone(),
            options.session_config.clone(),
            options.clean_start,
        )));

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
            pending_pubacks: Arc::new(Mutex::new(HashMap::new())),
            pending_pubcomps: Arc::new(Mutex::new(HashMap::new())),
            reconnect_attempt: 0,
            last_address: None,
            automatic_reconnect_lifecycle: AutomaticReconnectLifecycle::Armed,
            server_redirect: None,
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
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
                    QuicStreamManager::new(conn_arc, split.strategy)
                        .with_flow_headers(effective_flow_headers)
                        .with_flow_expire_interval(split.flow_expire_interval)
                        .with_flow_flags(split.flow_flags),
                ));
                (
                    UnifiedReader::quic(split.recv, protocol_version),
                    UnifiedWriter::Quic(split.send),
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
        self.set_connected(true);

        tracing::debug!("Starting background tasks (packet reader and keepalive)");
        self.start_background_tasks(reader, connection_epoch)?;
        tracing::debug!("Background tasks started successfully");

        if let Some(slots) = replay_slots {
            let replay = SessionReplay {
                items: replay_items,
                slots,
                session: Arc::clone(&self.session),
                writer: replay_writer,
                queued: Arc::clone(&self.queued_messages),
                policy: self.publish_policy(),
            };
            tokio::spawn(replay.run());
        }

        Ok(ConnectResult {
            session_present: connack.session_present,
        })
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
        self.ack_dispatcher.discard_pending();
        let session = self.session.read().await;
        session.discard_outbound_state().await;
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
    /// Returns `PacketTooLarge` when the message already exceeds the last known
    /// negotiated maximum packet size, so a `QoS` 1/2 publish is rejected at
    /// enqueue time instead of being acknowledged with `Ok` and then silently
    /// dropped when the queue is flushed on reconnect. A packet identifier is
    /// allocated only after the size check passes.
    async fn queue_publish_message(
        &self,
        topic: String,
        payload: Vec<u8>,
        options: &PublishOptions,
    ) -> Result<PublishResult> {
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

        let packet_id = self.allocate_packet_id().await?;
        publish.packet_id = Some(packet_id);
        self.queued_messages.lock().push_back(publish);
        Ok(PublishResult::QoS1Or2 { packet_id })
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

    async fn allocate_packet_id(&self) -> Result<u16> {
        self.session
            .read()
            .await
            .allocate_packet_id(&self.packet_id_generator, |packet_id| {
                self.pending_subacks.lock().contains_key(&packet_id)
                    || self.pending_unsubacks.lock().contains_key(&packet_id)
                    || self
                        .queued_messages
                        .lock()
                        .iter()
                        .any(|queued| queued.packet_id == Some(packet_id))
            })
            .await
            .ok_or(MqttError::PacketIdExhausted)
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

    fn setup_publish_acknowledgment(&self, qos: QoS, packet_id: Option<u16>) -> Option<PublishAck> {
        let pending = match qos {
            QoS::AtMostOnce => return None,
            QoS::AtLeastOnce => &self.pending_pubacks,
            QoS::ExactlyOnce => &self.pending_pubcomps,
        };
        let packet_id = packet_id?;
        let (tx, rx) = oneshot::channel();
        pending.lock().insert(packet_id, tx);
        Some(PublishAck {
            rx,
            packet_id,
            pending: Arc::clone(pending),
        })
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
        outbound::check_publish(&topic, &options)?;

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

        if let Some(alias) = options.properties.topic_alias {
            let session = self.session.read().await;
            let aliases = session.topic_alias_out().read().await;
            outbound::check_topic_alias(&aliases, &topic, alias)?;
        }
        let (final_payload, properties) = self.encode_payload(payload, &options)?;

        let mut publish = self.publish_policy().conform(PublishPacket {
            topic_name: topic,
            payload: final_payload,
            qos: options.qos,
            retain: options.retain,
            dup: false,
            packet_id: (options.qos != QoS::AtMostOnce).then_some(SIZE_PROBE_PACKET_ID),
            properties,
            protocol_version: self.options.protocol_version.as_u8(),
            stream_id: None,
        })?;

        self.check_publish_size(&publish).await?;

        if publish.qos != QoS::AtMostOnce {
            publish.packet_id = Some(self.allocate_packet_id().await?);
        }

        Ok(StagedPublish::Ready(publish))
    }

    pub(crate) async fn transmit_publish(
        &self,
        publish: PublishPacket,
    ) -> Result<Option<PublishAck>> {
        let qos = publish.qos;
        let packet_id = publish.packet_id;
        let flow = Arc::clone(self.session.read().await.flow_control());

        if !self.is_connected() {
            if let Some(pid) = packet_id {
                Self::release_send_quota(&flow, pid).await;
            }
            return Err(MqttError::NotConnected);
        }

        if qos != QoS::AtMostOnce {
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
                if let Some(pid) = packet_id {
                    Self::release_send_quota(&flow, pid).await;
                }
                return Err(e);
            }
        }

        let ack = self.setup_publish_acknowledgment(qos, packet_id);

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
        self.send_publish_packet(publish).await?;
        if let Some((alias, alias_topic)) = alias_mapping {
            self.record_outbound_topic_alias(alias, &alias_topic).await;
        }
        Ok(ack)
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

        let packet_id = self.allocate_packet_id().await?;
        let mut packet = packet;
        packet.packet_id = packet_id;

        let (tx, rx) = oneshot::channel();
        self.pending_subacks.lock().insert(packet_id, tx);

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

        let packet_id = self.allocate_packet_id().await?;
        let mut packet = packet;
        packet.packet_id = packet_id;

        let (tx, rx) = oneshot::channel();
        self.pending_unsubacks.lock().insert(packet_id, tx);

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

    fn start_background_tasks(
        &mut self,
        reader: UnifiedReader,
        connection_epoch: u64,
    ) -> Result<()> {
        let reader_session = self.session.clone();
        let reader_callbacks = self.callback_manager.clone();
        let suback_channels = self.pending_subacks.clone();
        let unsuback_channels = self.pending_unsubacks.clone();
        let puback_channels = self.pending_pubacks.clone();
        let pubcomp_channels = self.pending_pubcomps.clone();
        let writer_for_keepalive = self.writer.as_ref().ok_or(MqttError::NotConnected)?.clone();
        let lifecycle = keepalive::ConnectionLifecycle {
            connected: self.connected.clone(),
            connection_epoch,
            current_connection_epoch: self.connection_epoch.clone(),
            callbacks: Arc::clone(&self.connection_event_callbacks),
        };

        let writer_for_reader = writer_for_keepalive.clone();
        let keepalive_state = self.keepalive_state.clone();

        let ctx = PacketReaderContext {
            session: reader_session,
            callback_manager: reader_callbacks,
            suback_channels,
            unsuback_channels,
            puback_channels,
            pubcomp_channels,
            writer: writer_for_reader,
            lifecycle: lifecycle.clone(),
            #[cfg(feature = "transport-quic")]
            protocol_version: self.options.protocol_version.as_u8(),
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
        self.packet_reader_handle = Some(tokio::spawn(async move {
            tracing::debug!("📦 PACKET READER - Task starting");
            packet_reader_task_with_responses(reader, ctx_for_packet_reader).await;
            tracing::debug!("📦 PACKET READER - Task exited");
        }));

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
        assert!(
            matches!(
                within,
                Ok(StagedPublish::Queued(PublishResult::QoS1Or2 { .. }))
            ),
            "within-limit publish must queue: {within:?}"
        );
        assert_eq!(
            client.queued_messages.lock().len(),
            1,
            "within-limit publish must be queued"
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
}
