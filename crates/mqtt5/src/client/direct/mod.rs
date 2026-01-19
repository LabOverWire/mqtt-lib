//! Direct async client implementation
//!
//! This module implements the MQTT client using direct async calls.

mod handlers;
mod keepalive;
mod reader;
mod unified;

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
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
use crate::protocol::v5::properties::Properties;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::session::subscription::Subscription;
use crate::session::SessionState;
use crate::transport::flow::{FlowFlags, FlowId};
use crate::transport::{PacketIo, PacketWriter, QuicStreamManager, StreamStrategy, TransportType};
use crate::types::{ConnectOptions, ConnectResult, PublishOptions, PublishResult};
use crate::QoS;
use quinn::Connection;

#[cfg(feature = "opentelemetry")]
use crate::telemetry::propagation;

pub use unified::{UnifiedReader, UnifiedWriter};

use keepalive::{flow_expiration_task, keepalive_task_with_writer, KeepaliveState};
use reader::{packet_reader_task_with_responses, quic_stream_acceptor_task, PacketReaderContext};

pub struct DirectClientInner {
    pub writer: Option<Arc<tokio::sync::Mutex<UnifiedWriter>>>,
    pub quic_connection: Option<Arc<Connection>>,
    pub stream_strategy: Option<StreamStrategy>,
    pub quic_datagrams_enabled: bool,
    pub quic_stream_manager: Option<Arc<QuicStreamManager>>,
    pub session: Arc<tokio::sync::RwLock<SessionState>>,
    pub connected: Arc<AtomicBool>,
    pub callback_manager: Arc<CallbackManager>,
    pub packet_reader_handle: Option<JoinHandle<()>>,
    pub keepalive_handle: Option<JoinHandle<()>>,
    pub quic_stream_acceptor_handle: Option<JoinHandle<()>>,
    pub flow_expiration_handle: Option<JoinHandle<()>>,
    pub options: ConnectOptions,
    pub packet_id_generator: PacketIdGenerator,
    pub pending_subacks: Arc<Mutex<HashMap<u16, oneshot::Sender<SubAckPacket>>>>,
    pub pending_unsubacks: Arc<Mutex<HashMap<u16, oneshot::Sender<UnsubAckPacket>>>>,
    pub pending_pubacks: Arc<Mutex<HashMap<u16, oneshot::Sender<ReasonCode>>>>,
    pub pending_pubcomps: Arc<Mutex<HashMap<u16, oneshot::Sender<ReasonCode>>>>,
    pub reconnect_attempt: u32,
    pub last_address: Option<String>,
    pub queued_messages: Arc<Mutex<Vec<PublishPacket>>>,
    pub stored_subscriptions: Arc<Mutex<Vec<(String, SubscriptionOptions, CallbackId)>>>,
    pub queue_on_disconnect: bool,
    pub server_max_qos: Arc<Mutex<Option<u8>>>,
    pub auth_handler: Option<Arc<dyn AuthHandler>>,
    pub auth_method: Option<String>,
    pub keepalive_state: Arc<Mutex<KeepaliveState>>,
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

        Self {
            writer: None,
            quic_connection: None,
            stream_strategy: None,
            quic_datagrams_enabled: false,
            quic_stream_manager: None,
            session,
            connected: Arc::new(AtomicBool::new(false)),
            callback_manager: Arc::new(CallbackManager::new()),
            packet_reader_handle: None,
            keepalive_handle: None,
            quic_stream_acceptor_handle: None,
            flow_expiration_handle: None,
            options,
            packet_id_generator: PacketIdGenerator::new(),
            pending_subacks: Arc::new(Mutex::new(HashMap::new())),
            pending_unsubacks: Arc::new(Mutex::new(HashMap::new())),
            pending_pubacks: Arc::new(Mutex::new(HashMap::new())),
            pending_pubcomps: Arc::new(Mutex::new(HashMap::new())),
            reconnect_attempt: 0,
            last_address: None,
            queued_messages: Arc::new(Mutex::new(Vec::new())),
            stored_subscriptions: Arc::new(Mutex::new(Vec::new())),
            queue_on_disconnect,
            server_max_qos: Arc::new(Mutex::new(None)),
            auth_handler: None,
            auth_method,
            keepalive_state: Arc::new(Mutex::new(KeepaliveState::default())),
        }
    }

    pub fn set_auth_handler(&mut self, handler: impl AuthHandler + 'static) {
        self.auth_handler = Some(Arc::new(handler));
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::SeqCst);
    }

    pub fn is_queue_on_disconnect(&self) -> bool {
        self.queue_on_disconnect
    }

    pub fn set_queue_on_disconnect(&mut self, enabled: bool) {
        self.queue_on_disconnect = enabled;
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn send_packet(&mut self, packet: Packet) -> Result<()> {
        if let Some(writer) = &self.writer {
            let mut writer_guard = writer.lock().await;
            writer_guard.write_packet(packet).await?;
            Ok(())
        } else {
            Err(MqttError::NotConnected)
        }
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
                let handler = self
                    .auth_handler
                    .as_ref()
                    .ok_or(MqttError::AuthenticationFailed)?;

                let auth_method = auth.authentication_method().unwrap_or("");
                let auth_data = auth.authentication_data();

                let response = handler.handle_challenge(auth_method, auth_data).await?;

                match response {
                    AuthResponse::Continue(data) => {
                        let method = self.auth_method.clone().unwrap_or_default();
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
        let connect_packet = self.build_connect_packet().await;

        transport
            .write_packet(Packet::Connect(Box::new(connect_packet)))
            .await?;

        tracing::debug!("CLIENT: Waiting for CONNACK or AUTH");

        let connack = self.wait_for_connack(&mut transport).await?;

        if connack.reason_code != ReasonCode::Success {
            return Err(MqttError::ConnectionRefused(connack.reason_code));
        }

        if let Some(max_qos) = connack.properties.get_maximum_qos() {
            *self.server_max_qos.lock() = Some(max_qos);
            tracing::debug!("Server maximum QoS: {}", max_qos);
        } else {
            *self.server_max_qos.lock() = None;
        }

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
            TransportType::WebSocket(ws) => {
                let (r, w) = (*ws).into_split()?;
                (
                    UnifiedReader::websocket(r, protocol_version),
                    UnifiedWriter::WebSocket(w),
                )
            }
            TransportType::Quic(quic) => {
                let (w, r, conn, strategy, datagrams) = (*quic).into_split()?;
                let conn_arc = Arc::new(conn);
                self.quic_connection = Some(conn_arc.clone());
                self.stream_strategy = Some(strategy);
                self.quic_datagrams_enabled = datagrams;
                self.quic_stream_manager =
                    Some(Arc::new(QuicStreamManager::new(conn_arc, strategy)));
                (
                    UnifiedReader::quic(r, protocol_version),
                    UnifiedWriter::Quic(w),
                )
            }
        };

        self.writer = Some(Arc::new(tokio::sync::Mutex::new(writer)));
        self.set_connected(true);

        if let Some(max_packet_size) = self.options.properties.maximum_packet_size {
            self.session
                .write()
                .await
                .set_client_maximum_packet_size(max_packet_size)
                .await;
        }

        tracing::debug!("Starting background tasks (packet reader and keepalive)");
        self.start_background_tasks(reader)?;
        tracing::debug!("Background tasks started successfully");

        Ok(ConnectResult {
            session_present: connack.session_present,
        })
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
            return Ok(());
        }

        if send_disconnect {
            if let Some(ref writer) = self.writer {
                let disconnect = crate::packet::disconnect::DisconnectPacket {
                    reason_code: crate::protocol::v5::reason_codes::ReasonCode::Success,
                    properties: crate::protocol::v5::properties::Properties::default(),
                };
                let _ = writer
                    .lock()
                    .await
                    .write_packet(Packet::Disconnect(disconnect))
                    .await;
            }
        }

        self.stop_background_tasks();

        if let Some(manager) = self.quic_stream_manager.take() {
            manager.close_all_streams().await;
        }

        self.set_connected(false);
        self.writer = None;
        self.quic_connection = None;
        self.stream_strategy = None;
        self.quic_datagrams_enabled = false;

        Ok(())
    }

    fn queue_publish_message(
        &self,
        topic: String,
        payload: Vec<u8>,
        options: &PublishOptions,
    ) -> PublishResult {
        let packet_id = self.packet_id_generator.next();
        let publish = PublishPacket {
            topic_name: topic,
            packet_id: Some(packet_id),
            payload: payload.into(),
            qos: options.qos,
            retain: options.retain,
            dup: false,
            properties: options.properties.clone().into(),
            protocol_version: self.options.protocol_version.as_u8(),
        };

        self.queued_messages.lock().push(publish);
        PublishResult::QoS1Or2 { packet_id }
    }

    fn setup_publish_acknowledgment(
        &self,
        qos: QoS,
        packet_id: Option<u16>,
    ) -> Option<oneshot::Receiver<ReasonCode>> {
        match qos {
            QoS::AtMostOnce => None,
            QoS::AtLeastOnce => {
                let (tx, rx) = oneshot::channel();
                if let Some(pid) = packet_id {
                    self.pending_pubacks.lock().insert(pid, tx);
                }
                Some(rx)
            }
            QoS::ExactlyOnce => {
                let (tx, rx) = oneshot::channel();
                if let Some(pid) = packet_id {
                    self.pending_pubcomps.lock().insert(pid, tx);
                }
                Some(rx)
            }
        }
    }

    async fn wait_for_acknowledgment(
        &self,
        rx: oneshot::Receiver<ReasonCode>,
        qos: QoS,
        packet_id: Option<u16>,
    ) -> Result<()> {
        let timeout = Duration::from_secs(10);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(reason_code)) => {
                if reason_code.is_error() {
                    return Err(MqttError::PublishFailed(reason_code));
                }
                Ok(())
            }
            Ok(Err(_)) => Err(MqttError::ProtocolError(
                "Acknowledgment channel closed".to_string(),
            )),
            Err(_) => {
                if let Some(pid) = packet_id {
                    match qos {
                        QoS::AtLeastOnce => {
                            self.pending_pubacks.lock().remove(&pid);
                        }
                        QoS::ExactlyOnce => {
                            self.pending_pubcomps.lock().remove(&pid);
                        }
                        QoS::AtMostOnce => {}
                    }
                }
                Err(MqttError::Timeout)
            }
        }
    }

    /// # Errors
    ///
    /// Returns an error if the operation fails
    #[allow(clippy::too_many_lines)]
    pub async fn publish(
        &self,
        topic: String,
        payload: Vec<u8>,
        options: PublishOptions,
    ) -> Result<PublishResult> {
        if !self.is_connected() && self.queue_on_disconnect && options.qos != QoS::AtMostOnce {
            return Ok(self.queue_publish_message(topic, payload, &options));
        }

        let effective_qos = if let Some(max_qos) = *self.server_max_qos.lock() {
            let qos_value = match options.qos {
                QoS::AtMostOnce => 0,
                QoS::AtLeastOnce => 1,
                QoS::ExactlyOnce => 2,
            };
            if qos_value > max_qos {
                tracing::warn!(
                    "Requested QoS {} exceeds server maximum {}, using QoS {}",
                    qos_value,
                    max_qos,
                    max_qos
                );
                match max_qos {
                    0 => QoS::AtMostOnce,
                    1 => QoS::AtLeastOnce,
                    _ => QoS::ExactlyOnce,
                }
            } else {
                options.qos
            }
        } else {
            options.qos
        };

        let options = PublishOptions {
            qos: effective_qos,
            ..options
        };

        #[cfg(feature = "opentelemetry")]
        let options = {
            let mut opts = options;
            propagation::inject_trace_context(&mut opts.properties.user_properties);
            opts
        };

        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }

        let (final_payload, codec_content_type) = if options.skip_codec {
            (payload.into(), None)
        } else if let Some(ref registry) = self.options.codec_registry {
            registry.encode_with_default(&payload)?
        } else {
            (payload.into(), None)
        };

        let mut properties: Properties = options.properties.into();
        if let Some(ct) = codec_content_type {
            use crate::protocol::v5::properties::{PropertyId, PropertyValue};
            let _ = properties.add(PropertyId::ContentType, PropertyValue::Utf8String(ct));
        }

        let packet_id = (options.qos != QoS::AtMostOnce).then(|| self.packet_id_generator.next());

        let publish = PublishPacket {
            topic_name: topic,
            payload: final_payload,
            qos: options.qos,
            retain: options.retain,
            dup: false,
            packet_id,
            properties,
            protocol_version: self.options.protocol_version.as_u8(),
        };

        let mut buf = bytes::BytesMut::new();
        publish.encode(&mut buf)?;
        let packet_size = buf.len();
        self.session
            .read()
            .await
            .check_packet_size(packet_size)
            .await?;

        if options.qos != QoS::AtMostOnce {
            self.session
                .write()
                .await
                .store_unacked_publish(publish.clone())
                .await?;
        }

        let rx = self.setup_publish_acknowledgment(options.qos, packet_id);

        if publish.payload.len() > 10000 {
            tracing::debug!(
                topic = %publish.topic_name,
                payload_len = publish.payload.len(),
                packet_id = ?packet_id,
                qos = ?options.qos,
                "Sending large PUBLISH packet"
            );
        }

        self.send_publish_packet(publish, options.qos).await?;

        if let Some(rx) = rx {
            self.wait_for_acknowledgment(rx, options.qos, packet_id)
                .await?;
        }

        Ok(match packet_id {
            None => PublishResult::QoS0,
            Some(id) => PublishResult::QoS1Or2 { packet_id: id },
        })
    }

    async fn send_publish_packet(&self, publish: PublishPacket, qos: QoS) -> Result<()> {
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
                StreamStrategy::DataPerTopic => {
                    tracing::debug!(
                        topic = %publish.topic_name,
                        qos = ?qos,
                        "Using topic-specific QUIC stream for PUBLISH (DataPerTopic)"
                    );
                    manager
                        .send_on_topic_stream(publish.topic_name.clone(), Packet::Publish(publish))
                        .await?;
                    return Ok(());
                }
                StreamStrategy::DataPerSubscription => {
                    tracing::debug!(
                        topic = %publish.topic_name,
                        qos = ?qos,
                        "Using subscription-based QUIC stream for PUBLISH (DataPerSubscription)"
                    );
                    manager
                        .send_on_subscription_stream(
                            publish.topic_name.clone(),
                            Packet::Publish(publish),
                        )
                        .await?;
                    return Ok(());
                }
                StreamStrategy::ControlOnly => {}
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

    fn datagrams_available(&self) -> bool {
        self.quic_datagrams_enabled
            && self
                .quic_connection
                .as_ref()
                .and_then(|c| c.max_datagram_size())
                .is_some()
    }

    fn max_datagram_size(&self) -> Option<usize> {
        if !self.quic_datagrams_enabled {
            return None;
        }
        self.quic_connection
            .as_ref()
            .and_then(|c| c.max_datagram_size())
    }

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
        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }

        let writer = self.writer.as_ref().ok_or(MqttError::NotConnected)?;

        let packet_id = self.packet_id_generator.next();
        let mut packet = packet;
        packet.packet_id = packet_id;

        let (tx, rx) = oneshot::channel();
        self.pending_subacks.lock().insert(packet_id, tx);

        for filter in &packet.filters {
            self.stored_subscriptions.lock().push((
                filter.filter.clone(),
                filter.options,
                callback_id,
            ));
        }

        writer
            .lock()
            .await
            .write_packet(Packet::Subscribe(packet.clone()))
            .await?;

        let suback = self.wait_for_suback(rx, packet_id).await?;

        for (filter, reason_code) in packet.filters.iter().zip(suback.reason_codes.iter()) {
            if let Some(subscription) = Self::create_subscription_from_filter(filter, *reason_code)
            {
                self.session
                    .write()
                    .await
                    .add_subscription(filter.filter.clone(), subscription)
                    .await
                    .ok();
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

        let writer = self.writer.as_ref().ok_or(MqttError::NotConnected)?;

        let packet_id = self.packet_id_generator.next();
        let mut packet = packet;
        packet.packet_id = packet_id;

        let (tx, rx) = oneshot::channel();
        self.pending_unsubacks.lock().insert(packet_id, tx);

        {
            let mut stored = self.stored_subscriptions.lock();
            for topic in &packet.filters {
                stored.retain(|(stored_topic, _, _)| stored_topic != topic);
            }
        }

        writer
            .lock()
            .await
            .write_packet(Packet::Unsubscribe(packet.clone()))
            .await?;

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
            let _ = self
                .session
                .write()
                .await
                .remove_subscription(&filter)
                .await;
        }

        Ok(())
    }

    pub(crate) async fn build_connect_packet(&self) -> ConnectPacket {
        use crate::protocol::v5::properties::{PropertyId, PropertyValue};

        let session = self.session.read().await;

        let mut properties = Properties::default();

        if let Some(val) = self.options.properties.session_expiry_interval {
            let _ = properties.add(
                PropertyId::SessionExpiryInterval,
                PropertyValue::FourByteInteger(val),
            );
        }
        if let Some(val) = self.options.properties.receive_maximum {
            let _ = properties.add(
                PropertyId::ReceiveMaximum,
                PropertyValue::TwoByteInteger(val),
            );
        }
        if let Some(val) = self.options.properties.maximum_packet_size {
            let _ = properties.add(
                PropertyId::MaximumPacketSize,
                PropertyValue::FourByteInteger(val),
            );
        }
        if let Some(val) = self.options.properties.topic_alias_maximum {
            let _ = properties.add(
                PropertyId::TopicAliasMaximum,
                PropertyValue::TwoByteInteger(val),
            );
        }
        if let Some(ref method) = self.options.properties.authentication_method {
            let _ = properties.add(
                PropertyId::AuthenticationMethod,
                PropertyValue::Utf8String(method.clone()),
            );

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
                let _ = properties.add(
                    PropertyId::AuthenticationData,
                    PropertyValue::BinaryData(bytes::Bytes::from(data)),
                );
            }
        }

        let will_properties = Self::build_will_properties(self.options.will.as_ref());

        ConnectPacket {
            protocol_version: self.options.protocol_version.as_u8(),
            clean_start: self.options.clean_start,
            keep_alive: self
                .options
                .keep_alive
                .as_secs()
                .try_into()
                .unwrap_or(u16::MAX),
            client_id: session.client_id().to_string(),
            will: self.options.will.clone(),
            username: self.options.username.clone(),
            password: self.options.password.clone(),
            properties,
            will_properties,
        }
    }

    fn build_will_properties(will: Option<&crate::types::WillMessage>) -> Properties {
        use crate::protocol::v5::properties::{PropertyId, PropertyValue};

        let Some(will) = will else {
            return Properties::default();
        };

        let mut props = Properties::default();
        if let Some(val) = will.properties.will_delay_interval {
            let _ = props.add(
                PropertyId::WillDelayInterval,
                PropertyValue::FourByteInteger(val),
            );
        }
        if let Some(val) = will.properties.payload_format_indicator {
            let _ = props.add(
                PropertyId::PayloadFormatIndicator,
                PropertyValue::Byte(u8::from(val)),
            );
        }
        if let Some(val) = will.properties.message_expiry_interval {
            let _ = props.add(
                PropertyId::MessageExpiryInterval,
                PropertyValue::FourByteInteger(val),
            );
        }
        if let Some(ref val) = will.properties.content_type {
            let _ = props.add(
                PropertyId::ContentType,
                PropertyValue::Utf8String(val.clone()),
            );
        }
        if let Some(ref val) = will.properties.response_topic {
            let _ = props.add(
                PropertyId::ResponseTopic,
                PropertyValue::Utf8String(val.clone()),
            );
        }
        if let Some(ref val) = will.properties.correlation_data {
            let _ = props.add(
                PropertyId::CorrelationData,
                PropertyValue::BinaryData(bytes::Bytes::from(val.clone())),
            );
        }
        props
    }

    fn start_background_tasks(&mut self, reader: UnifiedReader) -> Result<()> {
        let reader_session = self.session.clone();
        let reader_callbacks = self.callback_manager.clone();
        let suback_channels = self.pending_subacks.clone();
        let unsuback_channels = self.pending_unsubacks.clone();
        let puback_channels = self.pending_pubacks.clone();
        let pubcomp_channels = self.pending_pubcomps.clone();
        let writer_for_keepalive = self.writer.as_ref().ok_or(MqttError::NotConnected)?.clone();
        let connected = self.connected.clone();

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
            connected,
            protocol_version: self.options.protocol_version.as_u8(),
            auth_handler: self.auth_handler.clone(),
            auth_method: self.auth_method.clone(),
            keepalive_state: keepalive_state.clone(),
            codec_registry: self.options.codec_registry.clone(),
        };

        let ctx_for_packet_reader = ctx.clone();
        self.packet_reader_handle = Some(tokio::spawn(async move {
            tracing::debug!("📦 PACKET READER - Task starting");
            packet_reader_task_with_responses(reader, ctx_for_packet_reader).await;
            tracing::debug!("📦 PACKET READER - Task exited");
        }));

        let keepalive_interval = self.options.keep_alive;
        if keepalive_interval.is_zero() {
            tracing::debug!("💓 KEEPALIVE - Disabled (interval is zero)");
        } else {
            let keepalive_writer = writer_for_keepalive;
            let keepalive_connected = self.connected.clone();
            let keepalive_config = self.options.keepalive_config;
            self.keepalive_handle = Some(tokio::spawn(async move {
                tracing::debug!("💓 KEEPALIVE - Task starting");
                keepalive_task_with_writer(
                    keepalive_writer,
                    keepalive_interval,
                    keepalive_state,
                    keepalive_connected,
                    keepalive_config,
                )
                .await;
                tracing::debug!("💓 KEEPALIVE - Task exited");
            }));
        }

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

    async fn get_recoverable_flows(&self) -> Vec<(FlowId, FlowFlags)> {
        self.session.read().await.get_recoverable_flows().await
    }

    pub(crate) async fn recover_flows(&self) -> Result<usize> {
        let Some(manager) = &self.quic_stream_manager else {
            return Ok(0);
        };

        let flows = self.get_recoverable_flows().await;
        let mut recovered = 0;

        for (flow_id, flags) in flows {
            let recovery_flags = FlowFlags { clean: 0, ..flags };

            match manager.open_recovery_stream(flow_id, recovery_flags).await {
                Ok((send, _recv)) => {
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

    fn stop_background_tasks(&mut self) {
        if let Some(handle) = self.packet_reader_handle.take() {
            handle.abort();
        }
        if let Some(handle) = self.keepalive_handle.take() {
            handle.abort();
        }
        if let Some(handle) = self.quic_stream_acceptor_handle.take() {
            handle.abort();
        }
        if let Some(handle) = self.flow_expiration_handle.take() {
            handle.abort();
        }
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
            .publish(
                "test/topic".to_string(),
                b"test payload".to_vec(),
                PublishOptions::default(),
            )
            .await;

        assert!(matches!(result, Err(MqttError::NotConnected)));
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
        assert!(result.is_ok());
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
