//! Client connection handler for the MQTT broker

mod auth;
mod connect;
mod lifecycle;
mod publish;
mod subscribe;

use crate::broker::auth::AuthProvider;
use crate::broker::config::BrokerConfig;
use crate::broker::events::{ClientConnectEvent, ClientDisconnectEvent};
use crate::broker::resource_monitor::ResourceMonitor;
use crate::broker::router::MessageRouter;
use crate::broker::storage::{ClientSession, DynamicStorage, StorageBackend};
use crate::broker::sys_topics::BrokerStats;
use crate::broker::transport::BrokerTransport;
use crate::error::{MqttError, Result};
use crate::packet::connect::ConnectPacket;
use crate::packet::disconnect::DisconnectPacket;
use crate::packet::publish::PublishPacket;
use crate::packet::Packet;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::time::Duration;
use crate::transport::packet_io::read_packet_reusing_buffer;
use crate::transport::PacketIo;
use bytes::{Bytes, BytesMut};
use mqtt5_protocol::KeepaliveConfig;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, timeout};
use tracing::{debug, info, warn};

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
    pub(super) publish_rx: flume::Receiver<PublishPacket>,
    pub(super) publish_tx: flume::Sender<PublishPacket>,
    pub(super) inflight_publishes: HashMap<u16, PublishPacket>,
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
    pub(super) external_packet_rx: Option<mpsc::Receiver<Packet>>,
    pub(super) client_receive_maximum: u16,
    pub(super) outbound_inflight: HashMap<u16, PublishPacket>,
    pub(super) protocol_version: u8,
    pub(super) write_buffer: BytesMut,
    pub(super) read_buffer: BytesMut,
    pub(super) skip_bridge_forwarding: bool,
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
            outbound_inflight: HashMap::new(),
            protocol_version: 5,
            write_buffer: BytesMut::with_capacity(4096),
            read_buffer: BytesMut::with_capacity(4096),
            skip_bridge_forwarding: false,
        }
    }

    #[must_use]
    pub fn with_skip_bridge_forwarding(mut self, skip: bool) -> Self {
        self.skip_bridge_forwarding = skip;
        self
    }

    pub(super) async fn route_publish(&self, publish: &PublishPacket, client_id: Option<&str>) {
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
            }
            Ok(Err(e)) => {
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

        let (disconnect_tx, mut disconnect_rx) = tokio::sync::oneshot::channel();

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

        let (result, session_taken_over) = if self.keep_alive.is_zero() {
            match self.handle_packets_no_keepalive(&mut disconnect_rx).await {
                Ok(taken_over) => (Ok(()), taken_over),
                Err(e) => (Err(e), false),
            }
        } else {
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

        let preserve_session = if let Some(ref session) = self.session {
            session.expiry_interval != Some(0)
        } else {
            false
        };

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

        self.resource_monitor
            .unregister_connection(&client_id, self.client_addr.ip())
            .await;

        if let Some(ref storage) = self.storage {
            if let Some(ref session) = self.session {
                match storage.get_session(&client_id).await {
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
                    if let Err(e) = storage.remove_session(&client_id).await {
                        warn!("Failed to remove session for {client_id}: {e}");
                    }
                    if let Err(e) = storage.remove_queued_messages(&client_id).await {
                        warn!("Failed to remove queued messages for {client_id}: {e}");
                    }
                    if let Err(e) = storage.remove_all_inflight_messages(&client_id).await {
                        warn!("Failed to remove inflight messages for {client_id}: {e}");
                    }
                    debug!(
                        "Removed session, queued, and inflight messages for client {}",
                        client_id
                    );
                }
            }
        }

        if let Some(ref user_id) = self.user_id {
            self.auth_provider.cleanup_session(user_id).await;
        }

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

    async fn handle_packets_no_keepalive(
        &mut self,
        disconnect_rx: &mut tokio::sync::oneshot::Receiver<()>,
    ) -> Result<bool> {
        loop {
            tokio::select! {
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

                _ = &mut *disconnect_rx => {
                    info!("Session taken over by another client");
                    return Ok(true);
                }
            }
        }
    }

    async fn handle_packets(
        &mut self,
        keep_alive_interval: &mut tokio::time::Interval,
        disconnect_rx: &mut tokio::sync::oneshot::Receiver<()>,
    ) -> Result<bool> {
        let mut last_packet_time = tokio::time::Instant::now();

        loop {
            tokio::select! {
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

                _ = keep_alive_interval.tick() => {
                    let elapsed = last_packet_time.elapsed();
                    let timeout_duration = KeepaliveConfig::default().timeout_duration(self.keep_alive);
                    if elapsed > timeout_duration {
                        warn!("Keep-alive timeout");
                        return Err(MqttError::KeepAliveTimeout);
                    }
                }

                _ = &mut *disconnect_rx => {
                    info!("Session taken over by another client");
                    return Ok(true);
                }

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
}

impl Drop for ClientHandler {
    fn drop(&mut self) {
        if let Some(ref client_id) = self.client_id {
            debug!("Client handler dropped for {}", client_id);
            self.stats.client_disconnected();
        }
    }
}
