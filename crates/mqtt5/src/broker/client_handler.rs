//! Client connection handler for the MQTT broker - simplified version

use crate::broker::auth::{AuthProvider, EnhancedAuthStatus};
use crate::broker::config::BrokerConfig;
use crate::broker::events::{
    ClientConnectEvent, ClientDisconnectEvent, ClientPublishEvent, ClientSubscribeEvent,
    ClientUnsubscribeEvent, MessageDeliveredEvent, SubAckReasonCode, SubscriptionInfo,
};
use crate::broker::resource_monitor::ResourceMonitor;
use crate::broker::router::MessageRouter;
use crate::broker::storage::{
    ClientSession, DynamicStorage, QueuedMessage, StorageBackend, StoredSubscription,
};
use crate::broker::sys_topics::BrokerStats;
use crate::broker::transport::BrokerTransport;
use crate::error::{MqttError, Result};
use crate::packet::auth::AuthPacket;
use crate::packet::connack::ConnAckPacket;
use crate::packet::connect::ConnectPacket;
use crate::packet::disconnect::DisconnectPacket;
use crate::packet::puback::PubAckPacket;
use crate::packet::pubcomp::PubCompPacket;
use crate::packet::publish::PublishPacket;
use crate::packet::pubrec::PubRecPacket;
use crate::packet::pubrel::PubRelPacket;
use crate::packet::suback::SubAckPacket;
use crate::packet::subscribe::SubscribePacket;
use crate::packet::unsuback::UnsubAckPacket;
use crate::packet::unsubscribe::UnsubscribePacket;
use crate::packet::Packet;
use crate::protocol::v5::properties::{PropertyId, PropertyValue};
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::time::Duration;
use crate::transport::packet_io::{encode_packet_to_buffer, read_packet_reusing_buffer, PacketIo};
use crate::types::ProtocolVersion;
use crate::QoS;
use crate::Transport;
use bytes::{Bytes, BytesMut};
use mqtt5_protocol::KeepaliveConfig;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::time::{interval, timeout};
use tracing::{debug, info, warn};

#[cfg(feature = "opentelemetry")]
use crate::telemetry::propagation;

#[derive(Debug, Clone, PartialEq)]
enum AuthState {
    NotStarted,
    InProgress,
    Completed,
}

struct PendingConnect {
    connect: ConnectPacket,
    assigned_client_id: Option<String>,
}

/// Handles a single client connection
#[allow(clippy::struct_excessive_bools)]
pub struct ClientHandler {
    transport: BrokerTransport,
    client_addr: SocketAddr,
    config: Arc<BrokerConfig>,
    router: Arc<MessageRouter>,
    auth_provider: Arc<dyn AuthProvider>,
    storage: Option<Arc<DynamicStorage>>,
    stats: Arc<BrokerStats>,
    resource_monitor: Arc<ResourceMonitor>,
    shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    client_id: Option<String>,
    user_id: Option<String>,
    keep_alive: Duration,
    publish_rx: flume::Receiver<PublishPacket>,
    publish_tx: flume::Sender<PublishPacket>,
    inflight_publishes: HashMap<u16, PublishPacket>,
    session: Option<ClientSession>,
    next_packet_id: u16,
    normal_disconnect: bool,
    disconnect_reason: Option<ReasonCode>,
    request_problem_information: bool,
    request_response_information: bool,
    auth_method: Option<String>,
    auth_state: AuthState,
    pending_connect: Option<PendingConnect>,
    topic_aliases: HashMap<u16, String>,
    external_packet_rx: Option<mpsc::Receiver<Packet>>,
    client_receive_maximum: u16,
    outbound_inflight: HashSet<u16>,
    protocol_version: u8,
    write_buffer: BytesMut,
    read_buffer: BytesMut,
    skip_bridge_forwarding: bool,
}

impl ClientHandler {
    /// Creates a new client handler
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

    /// Creates a new client handler with external packet channel for QUIC multi-stream
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
        external_packet_rx: Option<mpsc::Receiver<Packet>>,
    ) -> Self {
        let (publish_tx, publish_rx) = flume::bounded(config.client_channel_capacity);

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
            publish_rx,
            publish_tx,
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
            client_receive_maximum: 65535,
            outbound_inflight: HashSet::new(),
            protocol_version: 5,
            write_buffer: BytesMut::with_capacity(4096),
            read_buffer: BytesMut::with_capacity(4096),
            skip_bridge_forwarding: false,
        }
    }

    /// Configures this handler to skip forwarding messages to bridges.
    ///
    /// Use this for internal/replica connections where messages should only
    /// be routed to local subscribers and not forwarded to bridges.
    #[must_use]
    pub fn with_skip_bridge_forwarding(mut self, skip: bool) -> Self {
        self.skip_bridge_forwarding = skip;
        self
    }

    async fn route_publish(&self, publish: &PublishPacket, client_id: Option<&str>) {
        if self.skip_bridge_forwarding {
            self.router
                .route_message_local_only(publish, client_id)
                .await;
        } else {
            self.router.route_message(publish, client_id).await;
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
    #[allow(clippy::too_many_lines)]
    pub async fn run(mut self) -> Result<()> {
        // Wait for CONNECT packet
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
                // Successfully connected
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

                // Register connection with resource monitor
                self.resource_monitor
                    .register_connection(client_id.clone(), self.client_addr.ip())
                    .await;

                self.stats.client_connected();
            }
            Ok(Err(e)) => {
                // Log connection errors at appropriate level based on error type
                if e.to_string().contains("Connection closed") {
                    info!("Client disconnected during connect phase: {e}");
                    tracing::debug!("Connection closed error details: {:?}", e);
                } else {
                    warn!("Connect error: {e}");
                    tracing::debug!("Connect error details: {:?}", e);
                }
                return Err(e);
            }
            Err(_) => {
                warn!("Connect timeout from {}", self.client_addr);
                return Err(MqttError::Timeout);
            }
        }

        // Create disconnect channel for session takeover handling
        let (disconnect_tx, mut disconnect_rx) = tokio::sync::oneshot::channel();

        // Register with router
        let client_id = self.client_id.as_ref().unwrap().clone();
        self.router
            .register_client(client_id.clone(), self.publish_tx.clone(), disconnect_tx)
            .await;

        if let Some(ref handler) = self.config.event_handler {
            let event = ClientConnectEvent {
                client_id: client_id.clone().into(),
                clean_start: self
                    .pending_connect
                    .as_ref()
                    .is_none_or(|p| p.connect.clean_start),
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

        // Handle packets until disconnect
        let (result, session_taken_over) = if self.keep_alive.is_zero() {
            // No keepalive checking when keepalive is disabled
            match self.handle_packets_no_keepalive(&mut disconnect_rx).await {
                Ok(taken_over) => (Ok(()), taken_over),
                Err(e) => (Err(e), false),
            }
        } else {
            // Start keep-alive timer
            let mut keep_alive_interval = interval(self.keep_alive);
            keep_alive_interval.reset();
            match self
                .handle_packets(&mut keep_alive_interval, &mut disconnect_rx)
                .await
            {
                Ok(taken_over) => (Ok(()), taken_over),
                Err(e) => (Err(e), false),
            }
        };

        // Check if we should preserve the session
        let preserve_session = if let Some(ref session) = self.session {
            session.expiry_interval != Some(0)
        } else {
            false
        };

        // Only unregister client if not taken over

        if session_taken_over {
            info!(
                "Skipping unregister for client {} (session taken over)",
                client_id
            );
        } else {
            info!("Unregistering client {} (not taken over)", client_id);

            if preserve_session {
                self.router.disconnect_client(&client_id).await;
            } else {
                self.router.unregister_client(&client_id).await;
            }
        }

        // Unregister connection from resource monitor
        self.resource_monitor
            .unregister_connection(&client_id, self.client_addr.ip())
            .await;

        // Handle session cleanup based on session expiry
        if let Some(ref storage) = self.storage {
            if let Some(ref session) = self.session {
                // Update session last seen time
                if let Some(mut stored_session) =
                    storage.get_session(&client_id).await.ok().flatten()
                {
                    stored_session.touch();
                    storage.store_session(stored_session).await.ok();
                }

                // If session expiry is 0, remove session and queued messages
                if session.expiry_interval == Some(0) {
                    storage.remove_session(&client_id).await.ok();
                    storage.remove_queued_messages(&client_id).await.ok();
                    debug!(
                        "Removed session and queued messages for client {}",
                        client_id
                    );
                }
            }
        }

        // Clean up session-bound federated roles
        if let Some(ref user_id) = self.user_id {
            self.auth_provider.cleanup_session(user_id).await;
        }

        // Handle will message if this was an abnormal disconnect
        if !self.normal_disconnect {
            self.publish_will_message(&client_id).await;
        }

        if let Some(ref handler) = self.config.event_handler {
            let event = ClientDisconnectEvent {
                client_id: client_id.clone().into(),
                reason: self.disconnect_reason.unwrap_or(if self.normal_disconnect {
                    ReasonCode::Success
                } else {
                    ReasonCode::UnspecifiedError
                }),
                unexpected: !self.normal_disconnect,
            };
            handler.on_client_disconnect(event).await;
        }

        info!("Client {} disconnected", client_id);

        result
    }

    /// Waits for and processes CONNECT packet
    async fn wait_for_connect(&mut self) -> Result<()> {
        let packet =
            read_packet_reusing_buffer(&mut self.transport, 5, &mut self.read_buffer).await?;

        match packet {
            Packet::Connect(connect) => self.handle_connect(*connect).await,
            _ => Err(MqttError::ProtocolError(
                "Expected CONNECT packet".to_string(),
            )),
        }
    }

    /// Handles incoming packets without keepalive checking
    async fn handle_packets_no_keepalive(
        &mut self,
        disconnect_rx: &mut tokio::sync::oneshot::Receiver<()>,
    ) -> Result<bool> {
        loop {
            tokio::select! {
                // Read incoming packets from control stream
                packet_result = read_packet_reusing_buffer(&mut self.transport, self.protocol_version, &mut self.read_buffer) => {
                    match packet_result {
                        Ok(packet) => {
                            self.handle_packet(packet).await?;
                        }
                        Err(e) if e.is_normal_disconnect() => {
                            debug!("Client disconnected");
                            return Ok(false);
                        }
                        Err(e) => {
                            return Err(e);
                        }
                    }
                }

                // Read packets from external data streams (QUIC multi-stream)
                external_packet = async {
                    if let Some(ref mut rx) = self.external_packet_rx {
                        rx.recv().await
                    } else {
                        std::future::pending::<Option<Packet>>().await
                    }
                } => {
                    if let Some(packet) = external_packet {
                        self.handle_packet(packet).await?;
                    }
                }

                // Send outgoing publishes
                publish_result = self.publish_rx.recv_async() => {
                    if let Ok(publish) = publish_result {
                        self.send_publish(publish).await?;
                        while let Ok(more) = self.publish_rx.try_recv() {
                            self.send_publish(more).await?;
                        }
                    } else {
                        warn!("Publish channel closed unexpectedly");
                        return Ok(false);
                    }
                }

                // Session takeover
                _ = &mut *disconnect_rx => {
                    info!("Session taken over by another client");
                    return Ok(true);
                }
            }
        }
    }

    /// Handles incoming packets
    async fn handle_packets(
        &mut self,
        keep_alive_interval: &mut tokio::time::Interval,
        disconnect_rx: &mut tokio::sync::oneshot::Receiver<()>,
    ) -> Result<bool> {
        let mut last_packet_time = tokio::time::Instant::now();

        loop {
            tokio::select! {
                // Read incoming packets from control stream
                packet_result = read_packet_reusing_buffer(&mut self.transport, self.protocol_version, &mut self.read_buffer) => {
                    match packet_result {
                        Ok(packet) => {
                            last_packet_time = tokio::time::Instant::now();
                            self.handle_packet(packet).await?;
                        }
                        Err(e) if e.is_normal_disconnect() => {
                            debug!("Client disconnected");
                            return Ok(false);
                        }
                        Err(e) => {
                            return Err(e);
                        }
                    }
                }

                // Read packets from external data streams (QUIC multi-stream)
                external_packet = async {
                    if let Some(ref mut rx) = self.external_packet_rx {
                        rx.recv().await
                    } else {
                        std::future::pending::<Option<Packet>>().await
                    }
                } => {
                    if let Some(packet) = external_packet {
                        last_packet_time = tokio::time::Instant::now();
                        self.handle_packet(packet).await?;
                    }
                }

                // Send outgoing publishes
                publish_result = self.publish_rx.recv_async() => {
                    if let Ok(publish) = publish_result {
                        self.send_publish(publish).await?;
                        while let Ok(more) = self.publish_rx.try_recv() {
                            self.send_publish(more).await?;
                        }
                    } else {
                        warn!("Publish channel closed unexpectedly in handle_packets");
                        return Ok(false);
                    }
                }

                // Keep-alive check
                _ = keep_alive_interval.tick() => {
                    let elapsed = last_packet_time.elapsed();
                    let timeout_duration = KeepaliveConfig::default().timeout_duration(self.keep_alive);
                    if elapsed > timeout_duration {
                        warn!("Keep-alive timeout");
                        return Err(MqttError::KeepAliveTimeout);
                    }
                }

                // Session takeover
                _ = &mut *disconnect_rx => {
                    info!("Session taken over by another client");
                    return Ok(true);
                }

                // Shutdown signal
                _ = self.shutdown_rx.recv() => {
                    debug!("Shutdown signal received");
                    if self.protocol_version == 5 {
                        let disconnect = DisconnectPacket::new(ReasonCode::ServerShuttingDown);
                        let _ = self.transport.write_packet(Packet::Disconnect(disconnect)).await;
                    }
                    return Ok(false);
                }
            }
        }
    }

    /// Handles a single packet
    async fn handle_packet(&mut self, packet: Packet) -> Result<()> {
        match packet {
            Packet::Connect(_) => {
                if self.protocol_version == 5 {
                    let disconnect = DisconnectPacket::new(ReasonCode::ProtocolError);
                    self.transport
                        .write_packet(Packet::Disconnect(disconnect))
                        .await?;
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

    async fn validate_protocol_version(&mut self, protocol_version: u8) -> Result<()> {
        match protocol_version {
            4 | 5 => {
                self.protocol_version = protocol_version;
                debug!(
                    protocol_version,
                    addr = %self.client_addr,
                    "Client using MQTT v{}",
                    if protocol_version == 5 { "5.0" } else { "3.1.1" }
                );
                Ok(())
            }
            _ => {
                info!(
                    protocol_version,
                    addr = %self.client_addr,
                    "Rejecting connection: unsupported protocol version"
                );
                let connack = ConnAckPacket::new(false, ReasonCode::UnsupportedProtocolVersion);
                self.transport
                    .write_packet(Packet::ConnAck(connack))
                    .await?;
                Err(MqttError::UnsupportedProtocolVersion)
            }
        }
    }

    /// Handles CONNECT packet
    #[allow(clippy::too_many_lines)]
    async fn handle_connect(&mut self, mut connect: ConnectPacket) -> Result<()> {
        debug!(
            client_id = %connect.client_id,
            addr = %self.client_addr,
            protocol_version = connect.protocol_version,
            clean_start = connect.clean_start,
            keep_alive = connect.keep_alive,
            "Processing CONNECT packet"
        );

        self.validate_protocol_version(connect.protocol_version)
            .await?;

        self.request_problem_information = connect
            .properties
            .get_request_problem_information()
            .unwrap_or(true);
        self.request_response_information = connect
            .properties
            .get_request_response_information()
            .unwrap_or(false);

        // Handle empty client ID for MQTT v5
        let mut assigned_client_id = None;
        if connect.client_id.is_empty() {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let generated_id = format!("auto-{}", COUNTER.fetch_add(1, Ordering::SeqCst));
            debug!("Generated client ID '{}' for empty client ID", generated_id);
            connect.client_id.clone_from(&generated_id);
            assigned_client_id = Some(generated_id.clone());
        }

        // Check for enhanced authentication
        let auth_method_prop = connect.properties.get_authentication_method();
        let auth_data_prop = connect.properties.get_authentication_data();

        if let Some(method) = auth_method_prop {
            self.auth_method = Some(method.clone());

            if self.auth_provider.supports_enhanced_auth() {
                self.client_id = Some(connect.client_id.clone());

                let result = self
                    .auth_provider
                    .authenticate_enhanced(method, auth_data_prop, &connect.client_id)
                    .await?;

                match result.status {
                    EnhancedAuthStatus::Success => {
                        self.auth_state = AuthState::Completed;
                    }
                    EnhancedAuthStatus::Continue => {
                        self.auth_state = AuthState::InProgress;
                        self.keep_alive = Duration::from_secs(u64::from(connect.keep_alive));

                        let auth_packet = AuthPacket::continue_authentication(
                            result.auth_method,
                            result.auth_data,
                        )?;
                        self.transport
                            .write_packet(Packet::Auth(auth_packet))
                            .await?;

                        self.pending_connect = Some(PendingConnect {
                            connect,
                            assigned_client_id,
                        });

                        return Ok(());
                    }
                    EnhancedAuthStatus::Failed => {
                        let mut connack = if self.protocol_version == 4 {
                            ConnAckPacket::new_v311(false, result.reason_code)
                        } else {
                            ConnAckPacket::new(false, result.reason_code)
                        };
                        if self.protocol_version == 5 && self.request_problem_information {
                            if let Some(reason) = result.reason_string {
                                connack.properties.set_reason_string(reason);
                            }
                        }
                        self.transport
                            .write_packet(Packet::ConnAck(connack))
                            .await?;
                        return Err(MqttError::AuthenticationFailed);
                    }
                }
            } else {
                let mut connack = if self.protocol_version == 4 {
                    ConnAckPacket::new_v311(false, ReasonCode::BadAuthenticationMethod)
                } else {
                    ConnAckPacket::new(false, ReasonCode::BadAuthenticationMethod)
                };
                if self.protocol_version == 5 && self.request_problem_information {
                    connack.properties.set_reason_string(
                        "Server does not support enhanced authentication".to_string(),
                    );
                }
                self.transport
                    .write_packet(Packet::ConnAck(connack))
                    .await?;
                return Err(MqttError::AuthenticationFailed);
            }
        } else {
            // Simple authenticate
            let auth_result = self
                .auth_provider
                .authenticate(&connect, self.client_addr)
                .await?;

            if !auth_result.authenticated {
                debug!(
                    client_id = %connect.client_id,
                    reason = ?auth_result.reason_code,
                    "Authentication failed"
                );
                let mut connack = if self.protocol_version == 4 {
                    ConnAckPacket::new_v311(false, auth_result.reason_code)
                } else {
                    ConnAckPacket::new(false, auth_result.reason_code)
                };
                if self.protocol_version == 5 && self.request_problem_information {
                    connack
                        .properties
                        .set_reason_string("Authentication failed".to_string());
                }
                self.transport
                    .write_packet(Packet::ConnAck(connack))
                    .await?;
                return Err(MqttError::AuthenticationFailed);
            }

            self.user_id = auth_result.user_id;
        }

        // Store client info
        self.client_id = Some(connect.client_id.clone());
        self.keep_alive = Duration::from_secs(u64::from(connect.keep_alive));

        // Extract client's receive maximum (default 65535 per MQTT spec if not specified)
        self.client_receive_maximum = connect.properties.get_receive_maximum().unwrap_or(65535);
        debug!(
            client_id = %connect.client_id,
            receive_maximum = self.client_receive_maximum,
            "Client receive maximum"
        );

        // Handle session
        let session_present = self.handle_session(&connect).await?;

        // Send CONNACK
        let mut connack = if self.protocol_version == 4 {
            ConnAckPacket::new_v311(session_present, ReasonCode::Success)
        } else {
            ConnAckPacket::new(session_present, ReasonCode::Success)
        };

        // v5.0 only: set CONNACK properties
        if self.protocol_version == 5 {
            if let Some(ref assigned_id) = assigned_client_id {
                debug!("Setting assigned client ID in CONNACK: {}", assigned_id);
                connack
                    .properties
                    .set_assigned_client_identifier(assigned_id.clone());
            }

            connack
                .properties
                .set_topic_alias_maximum(self.config.topic_alias_maximum);
            connack
                .properties
                .set_retain_available(self.config.retain_available);
            connack.properties.set_maximum_packet_size(
                u32::try_from(self.config.max_packet_size).unwrap_or(u32::MAX),
            );
            connack
                .properties
                .set_wildcard_subscription_available(self.config.wildcard_subscription_available);
            connack.properties.set_subscription_identifier_available(
                self.config.subscription_identifier_available,
            );
            connack
                .properties
                .set_shared_subscription_available(self.config.shared_subscription_available);

            if self.config.maximum_qos < 2 {
                connack.properties.set_maximum_qos(self.config.maximum_qos);
            }

            if let Some(keep_alive) = self.config.server_keep_alive {
                connack
                    .properties
                    .set_server_keep_alive(u16::try_from(keep_alive.as_secs()).unwrap_or(u16::MAX));
            }

            if self.request_response_information {
                if let Some(ref response_info) = self.config.response_information {
                    connack
                        .properties
                        .set_response_information(response_info.clone());
                }
            }
        }

        debug!(
            client_id = %connect.client_id,
            session_present = session_present,
            assigned_client_id = ?assigned_client_id,
            "Sending CONNACK"
        );
        tracing::trace!("CONNACK properties: {:?}", connack.properties);
        self.transport
            .write_packet(Packet::ConnAck(connack))
            .await?;
        tracing::debug!("CONNACK sent successfully");

        // Deliver queued messages if session is present
        if session_present {
            self.deliver_queued_messages(&connect.client_id).await?;
        }

        Ok(())
    }

    /// Handles SUBSCRIBE packet
    #[allow(clippy::too_many_lines)]
    async fn handle_subscribe(&mut self, subscribe: SubscribePacket) -> Result<()> {
        let client_id = self.client_id.as_ref().unwrap();
        let mut reason_codes: Vec<crate::packet::suback::SubAckReasonCode> = Vec::new();

        for filter in &subscribe.filters {
            // Check authorization
            let authorized = self
                .auth_provider
                .authorize_subscribe(client_id, self.user_id.as_deref(), &filter.filter)
                .await?;

            if !authorized {
                reason_codes.push(crate::packet::suback::SubAckReasonCode::NotAuthorized);
                continue;
            }

            // Check subscription quota if limit is set
            if self.config.max_subscriptions_per_client > 0 {
                let is_existing = self
                    .router
                    .has_subscription(client_id, &filter.filter)
                    .await;
                if !is_existing {
                    let current_count = self.router.subscription_count_for_client(client_id).await;
                    if current_count >= self.config.max_subscriptions_per_client {
                        debug!(
                            "Client {} subscription quota exceeded ({}/{})",
                            client_id, current_count, self.config.max_subscriptions_per_client
                        );
                        reason_codes.push(crate::packet::suback::SubAckReasonCode::QuotaExceeded);
                        continue;
                    }
                }
            }

            // Check QoS limit
            let granted_qos = if filter.options.qos as u8 > self.config.maximum_qos {
                self.config.maximum_qos
            } else {
                filter.options.qos as u8
            };

            let is_new = self
                .router
                .subscribe(
                    client_id.clone(),
                    filter.filter.clone(),
                    QoS::from(granted_qos),
                    subscribe.properties.get_subscription_identifier(),
                    filter.options.no_local,
                    filter.options.retain_as_published,
                    filter.options.retain_handling as u8,
                    ProtocolVersion::try_from(self.protocol_version).unwrap_or_default(),
                )
                .await?;

            if let Some(ref mut session) = self.session {
                let stored = StoredSubscription {
                    qos: QoS::from(granted_qos),
                    no_local: filter.options.no_local,
                    retain_as_published: filter.options.retain_as_published,
                    retain_handling: filter.options.retain_handling as u8,
                    subscription_id: subscribe.properties.get_subscription_identifier(),
                    protocol_version: self.protocol_version,
                };
                session.add_subscription(filter.filter.clone(), stored);
                if let Some(ref storage) = self.storage {
                    storage.store_session(session.clone()).await.ok();
                }
            }

            let should_send_retained = match filter.options.retain_handling {
                crate::packet::subscribe::RetainHandling::SendAtSubscribe => true,
                crate::packet::subscribe::RetainHandling::SendAtSubscribeIfNew => is_new,
                crate::packet::subscribe::RetainHandling::DoNotSend => false,
            };

            if should_send_retained {
                let retained = self.router.get_retained_messages(&filter.filter).await;
                for mut msg in retained {
                    msg.retain = true;
                    self.publish_tx.send_async(msg).await.map_err(|_| {
                        MqttError::InvalidState("Failed to queue retained message".to_string())
                    })?;
                }
            }

            reason_codes.push(crate::packet::suback::SubAckReasonCode::from_qos(
                QoS::from(granted_qos),
            ));
        }

        let mut suback = if self.protocol_version == 4 {
            SubAckPacket::new_v311(subscribe.packet_id)
        } else {
            SubAckPacket::new(subscribe.packet_id)
        };
        suback.reason_codes.clone_from(&reason_codes);
        if self.protocol_version == 5 && self.request_problem_information {
            if reason_codes.contains(&crate::packet::suback::SubAckReasonCode::NotAuthorized) {
                suback
                    .properties
                    .set_reason_string("One or more subscriptions not authorized".to_string());
            } else if reason_codes.contains(&crate::packet::suback::SubAckReasonCode::QuotaExceeded)
            {
                suback
                    .properties
                    .set_reason_string("Subscription quota exceeded".to_string());
            }
        }
        let result = self.transport.write_packet(Packet::SubAck(suback)).await;

        if let Some(ref handler) = self.config.event_handler {
            let subscriptions: Vec<SubscriptionInfo> = reason_codes
                .iter()
                .zip(subscribe.filters.iter())
                .map(|(rc, filter)| SubscriptionInfo {
                    topic_filter: filter.filter.clone().into(),
                    qos: filter.options.qos,
                    result: SubAckReasonCode::from(*rc),
                })
                .collect();
            let event = ClientSubscribeEvent {
                client_id: client_id.clone().into(),
                subscriptions,
            };
            handler.on_client_subscribe(event).await;
        }

        result
    }

    /// Handles UNSUBSCRIBE packet
    async fn handle_unsubscribe(&mut self, unsubscribe: UnsubscribePacket) -> Result<()> {
        let client_id = self.client_id.as_ref().unwrap();
        let mut reason_codes = Vec::new();

        for topic_filter in &unsubscribe.filters {
            let removed = self.router.unsubscribe(client_id, topic_filter).await;

            // Remove from persisted session
            if removed {
                if let Some(ref mut session) = self.session {
                    session.remove_subscription(topic_filter);
                    if let Some(ref storage) = self.storage {
                        storage.store_session(session.clone()).await.ok();
                    }
                }
            }

            reason_codes.push(if removed {
                crate::packet::unsuback::UnsubAckReasonCode::Success
            } else {
                crate::packet::unsuback::UnsubAckReasonCode::NoSubscriptionExisted
            });
        }

        let mut unsuback = if self.protocol_version == 4 {
            UnsubAckPacket::new_v311(unsubscribe.packet_id)
        } else {
            UnsubAckPacket::new(unsubscribe.packet_id)
        };
        unsuback.reason_codes = reason_codes;
        let result = self
            .transport
            .write_packet(Packet::UnsubAck(unsuback))
            .await;

        if let Some(ref handler) = self.config.event_handler {
            let event = ClientUnsubscribeEvent {
                client_id: client_id.clone().into(),
                topic_filters: unsubscribe
                    .filters
                    .iter()
                    .map(|f| f.clone().into())
                    .collect(),
            };
            handler.on_client_unsubscribe(event).await;
        }

        result
    }

    #[cfg(feature = "opentelemetry")]
    async fn route_with_trace_context(
        &self,
        publish: &PublishPacket,
        client_id: &str,
    ) -> Result<()> {
        let user_props = propagation::extract_user_properties(&publish.properties);
        if let Some(span_context) = propagation::extract_trace_context(&user_props) {
            use opentelemetry::trace::TraceContextExt;
            use tracing::Instrument;
            use tracing_opentelemetry::OpenTelemetrySpanExt;

            let parent_cx =
                opentelemetry::Context::current().with_remote_span_context(span_context);
            let span = tracing::info_span!("broker_publish");
            let _ = span.set_parent(parent_cx);

            self.route_publish(publish, Some(client_id))
                .instrument(span)
                .await;
        } else {
            self.route_publish(publish, Some(client_id)).await;
        }
        Ok(())
    }

    /// Handles PUBLISH packet
    #[allow(clippy::too_many_lines)]
    async fn handle_publish(&mut self, mut publish: PublishPacket) -> Result<()> {
        let client_id = self.client_id.as_ref().unwrap();

        if let Some(alias) = publish.properties.get_topic_alias() {
            if alias == 0 || alias > self.config.topic_alias_maximum {
                return Err(MqttError::ProtocolError(format!(
                    "Invalid topic alias: {} (maximum: {})",
                    alias, self.config.topic_alias_maximum
                )));
            }

            if publish.topic_name.is_empty() {
                if let Some(topic) = self.topic_aliases.get(&alias) {
                    publish.topic_name.clone_from(topic);
                } else {
                    return Err(MqttError::ProtocolError(format!(
                        "Topic alias {alias} not found"
                    )));
                }
            } else {
                self.topic_aliases.insert(alias, publish.topic_name.clone());
            }
        } else if publish.topic_name.is_empty() {
            return Err(MqttError::ProtocolError(
                "Empty topic name without topic alias".to_string(),
            ));
        }

        let payload_size = publish.payload.len();
        self.stats.publish_received(payload_size);

        let authorized = self
            .auth_provider
            .authorize_publish(client_id, self.user_id.as_deref(), &publish.topic_name)
            .await?;

        if !authorized {
            warn!(
                "Client {} (user: {:?}) not authorized to publish to topic: {}",
                client_id, self.user_id, publish.topic_name
            );
            if publish.qos != QoS::AtMostOnce {
                // Send negative acknowledgment
                match publish.qos {
                    QoS::AtLeastOnce => {
                        let mut puback = PubAckPacket::new(publish.packet_id.unwrap());
                        puback.reason_code = ReasonCode::NotAuthorized;
                        if self.request_problem_information {
                            puback.properties.set_reason_string(format!(
                                "Not authorized to publish to topic: {}",
                                publish.topic_name
                            ));
                        }
                        debug!("Sending PUBACK with NotAuthorized");
                        self.transport.write_packet(Packet::PubAck(puback)).await?;
                    }
                    QoS::ExactlyOnce => {
                        let mut pubrec = PubRecPacket::new(publish.packet_id.unwrap());
                        pubrec.reason_code = ReasonCode::NotAuthorized;
                        if self.request_problem_information {
                            pubrec.properties.set_reason_string(format!(
                                "Not authorized to publish to topic: {}",
                                publish.topic_name
                            ));
                        }
                        debug!("Sending PUBREC with NotAuthorized");
                        self.transport.write_packet(Packet::PubRec(pubrec)).await?;
                    }
                    QoS::AtMostOnce => {}
                }
            }
            return Ok(());
        }

        if (publish.qos as u8) > self.config.maximum_qos {
            debug!(
                "Client {} sent QoS {} but max is {}",
                client_id, publish.qos as u8, self.config.maximum_qos
            );
            if let Some(packet_id) = publish.packet_id {
                match publish.qos {
                    QoS::AtLeastOnce => {
                        let mut puback = PubAckPacket::new(packet_id);
                        puback.reason_code = ReasonCode::QoSNotSupported;
                        if self.request_problem_information {
                            puback.properties.set_reason_string(format!(
                                "QoS {} not supported (maximum: {})",
                                publish.qos as u8, self.config.maximum_qos
                            ));
                        }
                        self.transport.write_packet(Packet::PubAck(puback)).await?;
                    }
                    QoS::ExactlyOnce => {
                        let mut pubrec = PubRecPacket::new(packet_id);
                        pubrec.reason_code = ReasonCode::QoSNotSupported;
                        if self.request_problem_information {
                            pubrec.properties.set_reason_string(format!(
                                "QoS {} not supported (maximum: {})",
                                publish.qos as u8, self.config.maximum_qos
                            ));
                        }
                        self.transport.write_packet(Packet::PubRec(pubrec)).await?;
                    }
                    QoS::AtMostOnce => {}
                }
            }
            return Ok(());
        }

        // Check retained message limits
        if publish.retain && !publish.payload.is_empty() {
            // Check retained message size limit
            if self.config.max_retained_message_size > 0
                && publish.payload.len() > self.config.max_retained_message_size
            {
                debug!(
                    "Client {} retained message too large ({} > {})",
                    client_id,
                    publish.payload.len(),
                    self.config.max_retained_message_size
                );
                if publish.qos != QoS::AtMostOnce {
                    match publish.qos {
                        QoS::AtLeastOnce => {
                            let mut puback = PubAckPacket::new(publish.packet_id.unwrap());
                            puback.reason_code = ReasonCode::QuotaExceeded;
                            if self.request_problem_information {
                                puback
                                    .properties
                                    .set_reason_string("Retained message too large".to_string());
                            }
                            self.transport.write_packet(Packet::PubAck(puback)).await?;
                        }
                        QoS::ExactlyOnce => {
                            let mut pubrec = PubRecPacket::new(publish.packet_id.unwrap());
                            pubrec.reason_code = ReasonCode::QuotaExceeded;
                            if self.request_problem_information {
                                pubrec
                                    .properties
                                    .set_reason_string("Retained message too large".to_string());
                            }
                            self.transport.write_packet(Packet::PubRec(pubrec)).await?;
                        }
                        QoS::AtMostOnce => {}
                    }
                }
                return Ok(());
            }

            // Check retained message count limit (only for new retained messages)
            if self.config.max_retained_messages > 0 {
                let is_update = self.router.has_retained_message(&publish.topic_name).await;
                if !is_update {
                    let current_count = self.router.retained_count().await;
                    if current_count >= self.config.max_retained_messages {
                        debug!(
                            "Client {} retained message limit exceeded ({}/{})",
                            client_id, current_count, self.config.max_retained_messages
                        );
                        if publish.qos != QoS::AtMostOnce {
                            match publish.qos {
                                QoS::AtLeastOnce => {
                                    let mut puback = PubAckPacket::new(publish.packet_id.unwrap());
                                    puback.reason_code = ReasonCode::QuotaExceeded;
                                    if self.request_problem_information {
                                        puback.properties.set_reason_string(
                                            "Retained message limit exceeded".to_string(),
                                        );
                                    }
                                    self.transport.write_packet(Packet::PubAck(puback)).await?;
                                }
                                QoS::ExactlyOnce => {
                                    let mut pubrec = PubRecPacket::new(publish.packet_id.unwrap());
                                    pubrec.reason_code = ReasonCode::QuotaExceeded;
                                    if self.request_problem_information {
                                        pubrec.properties.set_reason_string(
                                            "Retained message limit exceeded".to_string(),
                                        );
                                    }
                                    self.transport.write_packet(Packet::PubRec(pubrec)).await?;
                                }
                                QoS::AtMostOnce => {}
                            }
                        }
                        return Ok(());
                    }
                }
            }
        }

        // Check rate limits
        if !self
            .resource_monitor
            .can_send_message(client_id, payload_size)
            .await
        {
            warn!("Message from {} dropped due to rate limit", client_id);
            if publish.qos != QoS::AtMostOnce {
                // Send negative acknowledgment for rate limit exceeded
                match publish.qos {
                    QoS::AtLeastOnce => {
                        let mut puback = PubAckPacket::new(publish.packet_id.unwrap());
                        puback.reason_code = ReasonCode::QuotaExceeded;
                        if self.request_problem_information {
                            puback
                                .properties
                                .set_reason_string("Rate limit exceeded".to_string());
                        }
                        self.transport.write_packet(Packet::PubAck(puback)).await?;
                    }
                    QoS::ExactlyOnce => {
                        let mut pubrec = PubRecPacket::new(publish.packet_id.unwrap());
                        pubrec.reason_code = ReasonCode::QuotaExceeded;
                        if self.request_problem_information {
                            pubrec
                                .properties
                                .set_reason_string("Rate limit exceeded".to_string());
                        }
                        self.transport.write_packet(Packet::PubRec(pubrec)).await?;
                    }
                    QoS::AtMostOnce => {}
                }
            }
            return Ok(());
        }

        if let Some(ref handler) = self.config.event_handler {
            let response_topic = publish
                .properties
                .get(PropertyId::ResponseTopic)
                .and_then(|v| {
                    if let PropertyValue::Utf8String(s) = v {
                        Some(Arc::from(s.as_str()))
                    } else {
                        None
                    }
                });

            let correlation_data = publish
                .properties
                .get(PropertyId::CorrelationData)
                .and_then(|v| {
                    if let PropertyValue::BinaryData(b) = v {
                        Some(b.clone())
                    } else {
                        None
                    }
                });

            let event = ClientPublishEvent {
                client_id: client_id.clone().into(),
                topic: publish.topic_name.clone().into(),
                payload: publish.payload.clone(),
                qos: publish.qos,
                retain: publish.retain,
                packet_id: publish.packet_id,
                response_topic,
                correlation_data,
            };
            handler.on_client_publish(event).await;
        }

        match publish.qos {
            QoS::AtMostOnce => {
                #[cfg(feature = "opentelemetry")]
                self.route_with_trace_context(&publish, client_id).await?;
                #[cfg(not(feature = "opentelemetry"))]
                self.route_publish(&publish, Some(client_id)).await;
            }
            QoS::AtLeastOnce => {
                #[cfg(feature = "opentelemetry")]
                self.route_with_trace_context(&publish, client_id).await?;
                #[cfg(not(feature = "opentelemetry"))]
                self.route_publish(&publish, Some(client_id)).await;

                let mut puback = PubAckPacket::new(publish.packet_id.unwrap());
                puback.reason_code = ReasonCode::Success;
                self.transport.write_packet(Packet::PubAck(puback)).await?;
            }
            QoS::ExactlyOnce => {
                let packet_id = publish.packet_id.unwrap();
                self.inflight_publishes.insert(packet_id, publish);
                let mut pubrec = PubRecPacket::new(packet_id);
                pubrec.reason_code = ReasonCode::Success;
                self.transport.write_packet(Packet::PubRec(pubrec)).await?;
            }
        }

        Ok(())
    }

    async fn handle_puback(&mut self, puback: &PubAckPacket) {
        if self.outbound_inflight.remove(&puback.packet_id) {
            tracing::trace!(
                packet_id = puback.packet_id,
                inflight = self.outbound_inflight.len(),
                "Released outbound flow control quota on PUBACK"
            );

            if let Some(ref handler) = self.config.event_handler {
                if let Some(ref client_id) = self.client_id {
                    let event = MessageDeliveredEvent {
                        client_id: Arc::from(client_id.as_str()),
                        packet_id: puback.packet_id,
                        qos: QoS::AtLeastOnce,
                    };
                    handler.on_message_delivered(event).await;
                }
            }
        }
    }

    /// Handles PUBREC packet
    async fn handle_pubrec(&mut self, pubrec: PubRecPacket) -> Result<()> {
        // Send PUBREL
        let mut pub_rel = PubRelPacket::new(pubrec.packet_id);
        pub_rel.reason_code = ReasonCode::Success;
        self.transport.write_packet(Packet::PubRel(pub_rel)).await
    }

    /// Handles PUBREL packet
    async fn handle_pubrel(&mut self, pubrel: PubRelPacket) -> Result<()> {
        if let Some(publish) = self.inflight_publishes.remove(&pubrel.packet_id) {
            let client_id = self.client_id.as_ref().unwrap();

            #[cfg(feature = "opentelemetry")]
            self.route_with_trace_context(&publish, client_id).await?;
            #[cfg(not(feature = "opentelemetry"))]
            self.route_publish(&publish, Some(client_id)).await;
        }

        let mut pubcomp = PubCompPacket::new(pubrel.packet_id);
        pubcomp.reason_code = ReasonCode::Success;
        self.transport.write_packet(Packet::PubComp(pubcomp)).await
    }

    async fn handle_pubcomp(&mut self, pubcomp: &PubCompPacket) {
        if self.outbound_inflight.remove(&pubcomp.packet_id) {
            tracing::trace!(
                packet_id = pubcomp.packet_id,
                inflight = self.outbound_inflight.len(),
                "Released outbound flow control quota on PUBCOMP"
            );

            if let Some(ref handler) = self.config.event_handler {
                if let Some(ref client_id) = self.client_id {
                    let event = MessageDeliveredEvent {
                        client_id: Arc::from(client_id.as_str()),
                        packet_id: pubcomp.packet_id,
                        qos: QoS::ExactlyOnce,
                    };
                    handler.on_message_delivered(event).await;
                }
            }
        }
    }

    /// Handles PINGREQ packet
    async fn handle_pingreq(&mut self) -> Result<()> {
        self.transport.write_packet(Packet::PingResp).await
    }

    /// Handles session creation or restoration
    async fn handle_session(&mut self, connect: &ConnectPacket) -> Result<bool> {
        let mut session_present = false;
        if let Some(storage) = self.storage.clone() {
            // Get or create session
            let existing_session = storage.get_session(&connect.client_id).await?;

            if connect.clean_start || existing_session.is_none() {
                self.create_new_session(connect, &storage).await?;
            } else if let Some(session) = existing_session {
                session_present = true;
                self.restore_existing_session(connect, session, &storage)
                    .await?;
            }
        }
        Ok(session_present)
    }

    /// Creates a new session for the client
    async fn create_new_session(
        &mut self,
        connect: &ConnectPacket,
        storage: &Arc<DynamicStorage>,
    ) -> Result<()> {
        // Extract session expiry interval from properties
        let session_expiry = connect.properties.get_session_expiry_interval();

        // Extract will message if present
        let will_message = connect.will.clone();
        if let Some(ref will) = will_message {
            debug!(
                "Will message present with delay: {:?}",
                will.properties.will_delay_interval
            );
        }

        let mut session = ClientSession::new_with_will(
            connect.client_id.clone(),
            true, // persistent
            session_expiry,
            will_message,
        );
        session.receive_maximum = self.client_receive_maximum;
        debug!(
            "Created new session with will_delay_interval: {:?}",
            session.will_delay_interval
        );
        storage.store_session(session.clone()).await?;
        self.session = Some(session);
        Ok(())
    }

    /// Restores an existing session for the client
    async fn restore_existing_session(
        &mut self,
        connect: &ConnectPacket,
        mut session: ClientSession,
        storage: &Arc<DynamicStorage>,
    ) -> Result<()> {
        for (topic_filter, stored) in &session.subscriptions {
            self.router
                .subscribe(
                    connect.client_id.clone(),
                    topic_filter.clone(),
                    stored.qos,
                    stored.subscription_id,
                    stored.no_local,
                    stored.retain_as_published,
                    stored.retain_handling,
                    ProtocolVersion::try_from(self.protocol_version).unwrap_or_default(),
                )
                .await?;
        }

        // Update will message from new connection (replaces any existing will)
        session.will_message.clone_from(&connect.will);
        session.will_delay_interval = connect
            .will
            .as_ref()
            .and_then(|w| w.properties.will_delay_interval);

        // Update receive maximum from new connection
        session.receive_maximum = self.client_receive_maximum;

        // Update last seen
        session.touch();
        storage.store_session(session.clone()).await?;
        self.session = Some(session);
        Ok(())
    }

    /// Publishes will message on abnormal disconnect
    async fn publish_will_message(&self, client_id: &str) {
        if let Some(ref session) = self.session {
            if let Some(ref will) = session.will_message {
                debug!("Publishing will message for client {}", client_id);

                // Create publish packet from will message
                let mut publish =
                    PublishPacket::new(will.topic.clone(), will.payload.clone(), will.qos);
                publish.retain = will.retain;

                // Handle will delay interval
                if let Some(delay) = session.will_delay_interval {
                    debug!("Using will delay from session: {} seconds", delay);
                    if delay > 0 {
                        debug!("Spawning task to publish will after {} seconds", delay);
                        let router = Arc::clone(&self.router);
                        let publish_clone = publish.clone();
                        let client_id_clone = client_id.to_string();
                        let skip_bridges = self.skip_bridge_forwarding;
                        tokio::spawn(async move {
                            debug!(
                                "Task started: waiting {} seconds before publishing will for {}",
                                delay, client_id_clone
                            );
                            tokio::time::sleep(Duration::from_secs(u64::from(delay))).await;
                            debug!(
                                "Task completed: publishing delayed will message for {}",
                                client_id_clone
                            );
                            if skip_bridges {
                                router.route_message_local_only(&publish_clone, None).await;
                            } else {
                                router.route_message(&publish_clone, None).await;
                            }
                        });
                        debug!("Spawned delayed will task for {}", client_id);
                    } else {
                        debug!("Publishing will immediately (delay = 0)");
                        self.route_publish(&publish, None).await;
                    }
                } else {
                    debug!("Publishing will immediately (no delay specified)");
                    self.route_publish(&publish, None).await;
                }
            }
        }
    }

    /// Handles DISCONNECT packet
    fn handle_disconnect(&mut self, disconnect: &DisconnectPacket) -> Result<()> {
        self.normal_disconnect = true;
        self.disconnect_reason = Some(disconnect.reason_code);

        if let Some(ref mut session) = self.session {
            session.will_message = None;
            session.will_delay_interval = None;
        }

        Err(MqttError::ClientClosed)
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_auth(&mut self, auth: AuthPacket) -> Result<()> {
        let client_id = match &self.client_id {
            Some(id) => id.clone(),
            None => {
                return Err(MqttError::ProtocolError(
                    "AUTH received before CONNECT".to_string(),
                ));
            }
        };

        let auth_method = auth
            .authentication_method()
            .ok_or_else(|| {
                MqttError::ProtocolError("AUTH packet missing authentication method".to_string())
            })?
            .to_string();

        if let Some(ref expected_method) = self.auth_method {
            if auth_method != *expected_method {
                if self.protocol_version == 5 {
                    let disconnect = DisconnectPacket::new(ReasonCode::BadAuthenticationMethod);
                    self.transport
                        .write_packet(Packet::Disconnect(disconnect))
                        .await?;
                }
                return Err(MqttError::ProtocolError(
                    "Authentication method mismatch".to_string(),
                ));
            }
        }

        match auth.reason_code {
            ReasonCode::ContinueAuthentication => {
                let result = self
                    .auth_provider
                    .authenticate_enhanced(&auth_method, auth.authentication_data(), &client_id)
                    .await?;

                match result.status {
                    EnhancedAuthStatus::Success => {
                        self.auth_state = AuthState::Completed;

                        if let Some(pending) = self.pending_connect.take() {
                            let session_present = self.handle_session(&pending.connect).await?;

                            let mut connack = if self.protocol_version == 4 {
                                ConnAckPacket::new_v311(session_present, ReasonCode::Success)
                            } else {
                                ConnAckPacket::new(session_present, ReasonCode::Success)
                            };

                            if self.protocol_version == 5 {
                                if let Some(ref assigned_id) = pending.assigned_client_id {
                                    connack
                                        .properties
                                        .set_assigned_client_identifier(assigned_id.clone());
                                }

                                connack
                                    .properties
                                    .set_topic_alias_maximum(self.config.topic_alias_maximum);
                                connack
                                    .properties
                                    .set_retain_available(self.config.retain_available);
                                connack.properties.set_maximum_packet_size(
                                    u32::try_from(self.config.max_packet_size).unwrap_or(u32::MAX),
                                );
                            }

                            self.transport
                                .write_packet(Packet::ConnAck(connack))
                                .await?;

                            if session_present {
                                self.deliver_queued_messages(&pending.connect.client_id)
                                    .await?;
                            }
                        } else {
                            let success_auth = AuthPacket::success(result.auth_method)?;
                            self.transport
                                .write_packet(Packet::Auth(success_auth))
                                .await?;
                        }
                    }
                    EnhancedAuthStatus::Continue => {
                        let continue_auth = AuthPacket::continue_authentication(
                            result.auth_method,
                            result.auth_data,
                        )?;
                        self.transport
                            .write_packet(Packet::Auth(continue_auth))
                            .await?;
                    }
                    EnhancedAuthStatus::Failed => {
                        let failure_auth =
                            AuthPacket::failure(result.reason_code, result.reason_string)?;
                        self.transport
                            .write_packet(Packet::Auth(failure_auth))
                            .await?;
                        return Err(MqttError::AuthenticationFailed);
                    }
                }
            }
            ReasonCode::ReAuthenticate => {
                if self.auth_state != AuthState::Completed {
                    return Err(MqttError::ProtocolError(
                        "Cannot re-authenticate before initial auth completes".to_string(),
                    ));
                }

                let result = self
                    .auth_provider
                    .reauthenticate(
                        &auth_method,
                        auth.authentication_data(),
                        &client_id,
                        self.user_id.as_deref(),
                    )
                    .await?;

                match result.status {
                    EnhancedAuthStatus::Success => {
                        let success_auth = AuthPacket::success(result.auth_method)?;
                        self.transport
                            .write_packet(Packet::Auth(success_auth))
                            .await?;
                    }
                    EnhancedAuthStatus::Continue => {
                        let continue_auth = AuthPacket::continue_authentication(
                            result.auth_method,
                            result.auth_data,
                        )?;
                        self.transport
                            .write_packet(Packet::Auth(continue_auth))
                            .await?;
                    }
                    EnhancedAuthStatus::Failed => {
                        if self.protocol_version == 5 {
                            let disconnect = DisconnectPacket::new(result.reason_code);
                            self.transport
                                .write_packet(Packet::Disconnect(disconnect))
                                .await?;
                        }
                        return Err(MqttError::AuthenticationFailed);
                    }
                }
            }
            _ => {
                if self.protocol_version == 5 {
                    let disconnect = DisconnectPacket::new(ReasonCode::ProtocolError);
                    self.transport
                        .write_packet(Packet::Disconnect(disconnect))
                        .await?;
                }
                return Err(MqttError::ProtocolError(format!(
                    "Unexpected AUTH reason code: {:?}",
                    auth.reason_code
                )));
            }
        }

        Ok(())
    }

    /// Generate next packet ID
    fn next_packet_id(&mut self) -> u16 {
        let id = self.next_packet_id;
        self.next_packet_id = if self.next_packet_id == u16::MAX {
            1
        } else {
            self.next_packet_id + 1
        };
        id
    }

    /// Deliver queued messages to reconnected client
    async fn deliver_queued_messages(&mut self, client_id: &str) -> Result<()> {
        // Get messages and clear them from storage
        let queued_messages = if let Some(ref storage) = self.storage {
            let messages = storage.get_queued_messages(client_id).await?;
            storage.remove_queued_messages(client_id).await?;
            messages
        } else {
            Vec::new()
        };

        if !queued_messages.is_empty() {
            info!(
                "Delivering {} queued messages to {}",
                queued_messages.len(),
                client_id
            );

            // Convert messages and generate packet IDs
            for msg in queued_messages {
                let mut publish = msg.to_publish_packet();
                // Generate packet ID for QoS > 0
                if publish.qos != QoS::AtMostOnce && publish.packet_id.is_none() {
                    publish.packet_id = Some(self.next_packet_id());
                }
                if let Err(e) = self.publish_tx.try_send(publish) {
                    warn!("Failed to deliver queued message to {}: {:?}", client_id, e);
                }
            }
        }

        Ok(())
    }

    /// Sends a publish to the client
    async fn send_publish(&mut self, mut publish: PublishPacket) -> Result<()> {
        // Only enforce receive maximum for QoS > 0
        if publish.qos != QoS::AtMostOnce {
            // Check if we're at the client's receive maximum limit
            if self.outbound_inflight.len() >= usize::from(self.client_receive_maximum) {
                // Queue the message for later delivery when quota becomes available
                if let Some(ref storage) = self.storage {
                    let client_id = self.client_id.as_ref().unwrap();
                    let queued_msg =
                        QueuedMessage::new(publish.clone(), client_id.clone(), publish.qos, None);
                    storage.queue_message(queued_msg).await?;
                    debug!(
                        client_id = %client_id,
                        inflight = self.outbound_inflight.len(),
                        max = self.client_receive_maximum,
                        "Message queued due to receive maximum limit"
                    );
                }
                return Ok(());
            }

            // Assign packet ID if not already set
            if publish.packet_id.is_none() {
                publish.packet_id = Some(self.next_packet_id());
            }

            if let Some(packet_id) = publish.packet_id {
                self.outbound_inflight.insert(packet_id);
            }
        }

        let payload_size = publish.payload.len();
        self.write_buffer.clear();
        encode_packet_to_buffer(&Packet::Publish(publish), &mut self.write_buffer)?;
        self.transport.write(&self.write_buffer).await?;
        self.stats.publish_sent(payload_size);
        Ok(())
    }
}

impl Drop for ClientHandler {
    fn drop(&mut self) {
        // Clean up is handled in the run method after the main loop
        if let Some(ref client_id) = self.client_id {
            debug!("Client handler dropped for {}", client_id);
            self.stats.client_disconnected();
        }
    }
}
