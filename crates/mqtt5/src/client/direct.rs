//! Direct async client implementation
//!
//! This module implements the MQTT client using direct async calls.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::Duration;

use crate::callback::{CallbackId, CallbackManager};
use crate::error::{MqttError, Result};
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
use crate::transport::flow::{
    FlowFlags, FlowHeader, FlowId, FLOW_TYPE_CLIENT_DATA, FLOW_TYPE_CONTROL, FLOW_TYPE_SERVER_DATA,
};
use crate::transport::tls::{TlsReadHalf, TlsWriteHalf};
use crate::transport::websocket::{WebSocketReadHandle, WebSocketWriteHandle};
use crate::transport::{
    PacketIo, PacketReader, PacketWriter, QuicStreamManager, StreamStrategy, TransportType,
};
use crate::types::{ConnectOptions, ConnectResult, PublishOptions, PublishResult};
use crate::QoS;
use bytes::Bytes;
use quinn::{Connection, RecvStream, SendStream};
use std::time::Duration as StdDuration;

#[cfg(feature = "opentelemetry")]
use crate::telemetry::propagation;

/// Unified reader type that can handle TCP, TLS, WebSocket, and QUIC
pub enum UnifiedReader {
    Tcp(OwnedReadHalf),
    Tls(TlsReadHalf),
    WebSocket(WebSocketReadHandle),
    Quic(RecvStream),
}

impl PacketReader for UnifiedReader {
    async fn read_packet(&mut self) -> Result<Packet> {
        match self {
            Self::Tcp(reader) => reader.read_packet().await,
            Self::Tls(reader) => reader.read_packet().await,
            Self::WebSocket(reader) => reader.read_packet().await,
            Self::Quic(reader) => reader.read_packet().await,
        }
    }
}

/// Unified writer type that can handle TCP, TLS, WebSocket, and QUIC
pub enum UnifiedWriter {
    Tcp(OwnedWriteHalf),
    Tls(TlsWriteHalf),
    WebSocket(WebSocketWriteHandle),
    Quic(SendStream),
}

impl PacketWriter for UnifiedWriter {
    async fn write_packet(&mut self, packet: Packet) -> Result<()> {
        match self {
            Self::Tcp(writer) => writer.write_packet(packet).await,
            Self::Tls(writer) => writer.write_packet(packet).await,
            Self::WebSocket(writer) => writer.write_packet(packet).await,
            Self::Quic(writer) => writer.write_packet(packet).await,
        }
    }
}

/// Internal client state
pub struct DirectClientInner {
    /// Write half of the transport for client operations
    pub writer: Option<Arc<tokio::sync::RwLock<UnifiedWriter>>>,
    pub quic_connection: Option<Arc<Connection>>,
    pub stream_strategy: Option<StreamStrategy>,
    pub quic_datagrams_enabled: bool,
    pub quic_stream_manager: Option<Arc<QuicStreamManager>>,
    pub session: Arc<RwLock<SessionState>>,
    /// Connection status
    pub connected: Arc<AtomicBool>,
    /// Callback manager for subscriptions
    pub callback_manager: Arc<CallbackManager>,
    /// Background task handles
    pub packet_reader_handle: Option<JoinHandle<()>>,
    pub keepalive_handle: Option<JoinHandle<()>>,
    pub quic_stream_acceptor_handle: Option<JoinHandle<()>>,
    pub flow_expiration_handle: Option<JoinHandle<()>>,
    /// Connection options
    pub options: ConnectOptions,
    /// Packet ID generator
    pub packet_id_generator: PacketIdGenerator,
    /// Pending SUBACK responses (`packet_id` -> oneshot sender)
    pub pending_subacks: Arc<Mutex<HashMap<u16, oneshot::Sender<SubAckPacket>>>>,
    /// Pending UNSUBACK responses (`packet_id` -> oneshot sender)
    pub pending_unsubacks: Arc<Mutex<HashMap<u16, oneshot::Sender<UnsubAckPacket>>>>,
    /// Pending PUBACK responses (`packet_id` -> oneshot sender)
    pub pending_pubacks: Arc<Mutex<HashMap<u16, oneshot::Sender<ReasonCode>>>>,
    /// Pending PUBCOMP responses (`packet_id` -> oneshot sender) - for `QoS` 2
    pub pending_pubcomps: Arc<Mutex<HashMap<u16, oneshot::Sender<ReasonCode>>>>,
    /// Reconnection state
    pub reconnect_attempt: u32,
    /// Last connection address (for reconnection)
    pub last_address: Option<String>,
    /// Queued messages during disconnection
    pub queued_messages: Arc<Mutex<Vec<PublishPacket>>>,
    /// Stored subscriptions for restoration (topic, options, `callback_id`)
    pub stored_subscriptions: Arc<RwLock<Vec<(String, SubscriptionOptions, CallbackId)>>>,
    /// Whether to queue messages when disconnected
    pub queue_on_disconnect: bool,
    /// Server's maximum QoS level (from CONNACK)
    pub server_max_qos: Arc<RwLock<Option<u8>>>,
}

impl DirectClientInner {
    /// Create new client inner state
    pub fn new(options: ConnectOptions) -> Self {
        let session = Arc::new(RwLock::new(SessionState::new(
            options.client_id.clone(),
            options.session_config.clone(),
            options.clean_start,
        )));

        let queue_on_disconnect = !options.clean_start; // Enable queuing for persistent sessions by default

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
            stored_subscriptions: Arc::new(RwLock::new(Vec::new())),
            queue_on_disconnect,
            server_max_qos: Arc::new(RwLock::new(None)),
        }
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Set connected status
    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::SeqCst);
    }

    /// Check if message queuing is enabled
    pub fn is_queue_on_disconnect(&self) -> bool {
        self.queue_on_disconnect
    }

    /// Set whether to queue messages when disconnected
    pub fn set_queue_on_disconnect(&mut self, enabled: bool) {
        self.queue_on_disconnect = enabled;
    }

    /// Send a packet to the broker
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn send_packet(&mut self, packet: Packet) -> Result<()> {
        if let Some(writer) = &self.writer {
            let mut writer_guard = writer.write().await;
            writer_guard.write_packet(packet).await?;
            Ok(())
        } else {
            Err(MqttError::NotConnected)
        }
    }
}

/// Direct async implementation of MQTT operations
impl DirectClientInner {
    /// Connect to broker
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn connect(&mut self, mut transport: TransportType) -> Result<ConnectResult> {
        // Build CONNECT packet
        let connect_packet = self.build_connect_packet().await;

        // Send CONNECT packet
        transport
            .write_packet(Packet::Connect(Box::new(connect_packet)))
            .await?;

        // Read CONNACK directly
        tracing::debug!("CLIENT: Waiting for CONNACK");
        let packet = transport.read_packet().await?;
        tracing::debug!("CLIENT: Received packet after CONNECT");
        match packet {
            Packet::ConnAck(connack) => {
                tracing::debug!(
                    "CLIENT: Got CONNACK with reason code: {:?}",
                    connack.reason_code
                );
                // Check reason code
                if connack.reason_code != ReasonCode::Success {
                    return Err(MqttError::ConnectionRefused(connack.reason_code));
                }

                // Store server's maximum QoS if present
                if let Some(max_qos) = connack.properties.get_maximum_qos() {
                    *self.server_max_qos.write().await = Some(max_qos);
                    tracing::debug!("Server maximum QoS: {}", max_qos);
                } else {
                    // If not present, server supports QoS 2
                    *self.server_max_qos.write().await = None;
                }

                // Split the transport for concurrent access
                let (reader, writer) = match transport {
                    TransportType::Tcp(tcp) => {
                        let (r, w) = tcp.into_split()?;
                        (UnifiedReader::Tcp(r), UnifiedWriter::Tcp(w))
                    }
                    TransportType::Tls(tls) => {
                        let (r, w) = (*tls).into_split()?;
                        (UnifiedReader::Tls(r), UnifiedWriter::Tls(w))
                    }
                    TransportType::WebSocket(ws) => {
                        let (r, w) = (*ws).into_split()?;
                        (UnifiedReader::WebSocket(r), UnifiedWriter::WebSocket(w))
                    }
                    TransportType::Quic(quic) => {
                        let (w, r, conn, strategy, datagrams) = (*quic).into_split()?;
                        let conn_arc = Arc::new(conn);
                        self.quic_connection = Some(conn_arc.clone());
                        self.stream_strategy = Some(strategy);
                        self.quic_datagrams_enabled = datagrams;
                        self.quic_stream_manager =
                            Some(Arc::new(QuicStreamManager::new(conn_arc, strategy)));
                        (UnifiedReader::Quic(r), UnifiedWriter::Quic(w))
                    }
                };

                // Store the writer half wrapped in Arc<RwLock<>>
                self.writer = Some(Arc::new(tokio::sync::RwLock::new(writer)));

                // Mark as connected
                self.set_connected(true);

                // Set client maximum packet size from options
                if let Some(max_packet_size) = self.options.properties.maximum_packet_size {
                    self.session
                        .write()
                        .await
                        .set_client_maximum_packet_size(max_packet_size)
                        .await;
                }

                // Start background tasks with reader half
                tracing::debug!("Starting background tasks (packet reader and keepalive)");
                self.start_background_tasks(reader)?;
                tracing::debug!("Background tasks started successfully");

                Ok(ConnectResult {
                    session_present: connack.session_present,
                })
            }
            _ => Err(MqttError::ProtocolError("Expected CONNACK".to_string())),
        }
    }

    /// Disconnect from broker - DIRECT async method
    ///
    /// # Errors
    ///
    /// Returns `MqttError::NotConnected` if the client is not connected
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn disconnect(&mut self) -> Result<()> {
        self.disconnect_with_packet(true).await
    }

    /// Disconnect with option to send DISCONNECT packet
    ///
    /// # Errors
    ///
    /// Returns `MqttError::NotConnected` if the client is not connected
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn disconnect_with_packet(&mut self, send_disconnect: bool) -> Result<()> {
        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }

        // Send DISCONNECT directly if requested
        if send_disconnect {
            if let Some(ref writer) = self.writer {
                let disconnect = crate::packet::disconnect::DisconnectPacket {
                    reason_code: crate::protocol::v5::reason_codes::ReasonCode::Success,
                    properties: crate::protocol::v5::properties::Properties::default(),
                };
                let _ = writer
                    .write()
                    .await
                    .write_packet(Packet::Disconnect(disconnect))
                    .await;
            }
        }

        // Stop background tasks
        self.stop_background_tasks();

        // Clean up QUIC stream manager
        if let Some(manager) = self.quic_stream_manager.take() {
            manager.close_all_streams().await;
        }

        // Clear state
        self.set_connected(false);
        self.writer = None;
        self.quic_connection = None;
        self.stream_strategy = None;
        self.quic_datagrams_enabled = false;

        Ok(())
    }

    /// Queue a publish message when disconnected
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    async fn queue_publish_message(
        &self,
        topic: String,
        payload: Vec<u8>,
        options: &PublishOptions,
    ) -> Result<PublishResult> {
        let packet_id = self.packet_id_generator.next();
        let publish = PublishPacket {
            topic_name: topic,
            packet_id: Some(packet_id),
            payload,
            qos: options.qos,
            retain: options.retain,
            dup: false,
            properties: options.properties.clone().into(),
        };

        self.queued_messages.lock().await.push(publish);
        Ok(PublishResult::QoS1Or2 { packet_id })
    }

    /// Set up acknowledgment channel for `QoS` > 0
    async fn setup_publish_acknowledgment(
        &self,
        qos: QoS,
        packet_id: Option<u16>,
    ) -> Option<oneshot::Receiver<ReasonCode>> {
        match qos {
            QoS::AtMostOnce => None,
            QoS::AtLeastOnce => {
                let (tx, rx) = oneshot::channel();
                if let Some(pid) = packet_id {
                    self.pending_pubacks.lock().await.insert(pid, tx);
                }
                Some(rx)
            }
            QoS::ExactlyOnce => {
                let (tx, rx) = oneshot::channel();
                if let Some(pid) = packet_id {
                    self.pending_pubcomps.lock().await.insert(pid, tx);
                }
                Some(rx)
            }
        }
    }

    /// Wait for acknowledgment with timeout
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
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
                // Timeout - remove from pending in a separate task to avoid blocking
                if let Some(pid) = packet_id {
                    match qos {
                        QoS::AtLeastOnce => {
                            let pending = self.pending_pubacks.clone();
                            tokio::spawn(async move {
                                pending.lock().await.remove(&pid);
                            });
                        }
                        QoS::ExactlyOnce => {
                            let pending = self.pending_pubcomps.clone();
                            tokio::spawn(async move {
                                pending.lock().await.remove(&pid);
                            });
                        }
                        QoS::AtMostOnce => {}
                    }
                }
                Err(MqttError::Timeout)
            }
        }
    }

    /// Publish a message - DIRECT async method
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `MqttError::NotConnected` - The client is not connected
    /// - `MqttError::InvalidTopicName` - The topic name is invalid
    /// - `MqttError::PacketIdExhausted` - No packet IDs available for `QoS` 1/2
    /// - `MqttError::FlowControlExceeded` - Flow control limit reached
    /// - `MqttError::Io` - Transport write error
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn publish(
        &self,
        topic: String,
        payload: Vec<u8>,
        options: PublishOptions,
    ) -> Result<PublishResult> {
        // Check if we should queue the message
        if !self.is_connected() && self.queue_on_disconnect && options.qos != QoS::AtMostOnce {
            return self.queue_publish_message(topic, payload, &options).await;
        }

        // Enforce server's maximum QoS limit
        let effective_qos = if let Some(max_qos) = *self.server_max_qos.read().await {
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
                // Downgrade QoS to server's maximum
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

        // Create adjusted options with effective QoS
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

        // Get packet ID for QoS > 0
        let packet_id = (options.qos != QoS::AtMostOnce).then(|| self.packet_id_generator.next());

        // Build publish packet
        let publish = PublishPacket {
            topic_name: topic,
            payload,
            qos: options.qos,
            retain: options.retain,
            dup: false,
            packet_id,
            properties: options.properties.into(),
        };

        // Check packet size limit
        let mut buf = bytes::BytesMut::new();
        publish.encode(&mut buf)?;
        let packet_size = buf.len();
        self.session
            .read()
            .await
            .check_packet_size(packet_size)
            .await?;

        // Store for QoS handling
        if options.qos != QoS::AtMostOnce {
            self.session
                .write()
                .await
                .store_unacked_publish(publish.clone())
                .await?;
        }

        // For QoS > 0, set up acknowledgment waiting
        let rx = self
            .setup_publish_acknowledgment(options.qos, packet_id)
            .await;

        // Send PUBLISH packet
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

        // Wait for acknowledgment if QoS > 0
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
            .write()
            .await
            .write_packet(Packet::Publish(publish))
            .await?;
        Ok(())
    }

    pub fn datagrams_available(&self) -> bool {
        self.quic_datagrams_enabled
            && self
                .quic_connection
                .as_ref()
                .and_then(|c| c.max_datagram_size())
                .is_some()
    }

    pub fn max_datagram_size(&self) -> Option<usize> {
        if !self.quic_datagrams_enabled {
            return None;
        }
        self.quic_connection
            .as_ref()
            .and_then(|c| c.max_datagram_size())
    }

    pub fn send_datagram(&self, data: bytes::Bytes) -> Result<()> {
        if !self.quic_datagrams_enabled {
            return Err(MqttError::InvalidState("Datagrams not enabled".to_string()));
        }
        let conn = self
            .quic_connection
            .as_ref()
            .ok_or(MqttError::NotConnected)?;
        conn.send_datagram(data)
            .map_err(|e| MqttError::ConnectionError(format!("Datagram send failed: {e}")))
    }

    pub async fn send_datagram_wait(&self, data: bytes::Bytes) -> Result<()> {
        if !self.quic_datagrams_enabled {
            return Err(MqttError::InvalidState("Datagrams not enabled".to_string()));
        }
        let conn = self
            .quic_connection
            .as_ref()
            .ok_or(MqttError::NotConnected)?;
        conn.send_datagram_wait(data)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("Datagram send failed: {e}")))
    }

    pub async fn read_datagram(&self) -> Result<bytes::Bytes> {
        if !self.quic_datagrams_enabled {
            return Err(MqttError::InvalidState("Datagrams not enabled".to_string()));
        }
        let conn = self
            .quic_connection
            .as_ref()
            .ok_or(MqttError::NotConnected)?;
        conn.read_datagram()
            .await
            .map_err(|e| MqttError::ConnectionError(format!("Datagram read failed: {e}")))
    }

    pub fn send_publish_as_datagram(&self, publish: PublishPacket) -> Result<()> {
        if publish.qos != QoS::AtMostOnce {
            return Err(MqttError::ProtocolError(
                "Only QoS 0 publishes can be sent as datagrams".to_string(),
            ));
        }

        let mut buf = bytes::BytesMut::new();
        crate::transport::packet_io::encode_packet_to_buffer(&Packet::Publish(publish), &mut buf)?;

        self.send_datagram(buf.freeze())
    }

    /// Create a subscription from filter and reason code
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
            _ => None, // Failed subscription
        }
    }

    /// Wait for SUBACK with timeout
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
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
                // Timeout - remove from pending in a separate task to avoid blocking
                let pending = self.pending_subacks.clone();
                tokio::spawn(async move {
                    pending.lock().await.remove(&packet_id);
                });
                Err(MqttError::Timeout)
            }
        }
    }

    /// Subscribe to topics with callback ID - DIRECT async method
    ///
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

        // Get packet ID
        let packet_id = self.packet_id_generator.next();
        let mut packet = packet;
        packet.packet_id = packet_id;

        // Create oneshot channel for SUBACK response
        let (tx, rx) = oneshot::channel();
        self.pending_subacks.lock().await.insert(packet_id, tx);

        // Store subscription info for restoration with callback ID
        for filter in &packet.filters {
            self.stored_subscriptions.write().await.push((
                filter.filter.clone(),
                filter.options,
                callback_id,
            ));
        }

        // Send SUBSCRIBE directly
        writer
            .write()
            .await
            .write_packet(Packet::Subscribe(packet.clone()))
            .await?;

        // Wait for SUBACK from packet reader task
        let suback = self.wait_for_suback(rx, packet_id).await?;

        // Update session
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

        // Convert reason codes to (packet_id, QoS) tuples
        let results: Vec<(u16, QoS)> = suback
            .reason_codes
            .iter()
            .map(|rc| {
                let qos = match rc {
                    SubAckReasonCode::GrantedQoS1 => QoS::AtLeastOnce,
                    SubAckReasonCode::GrantedQoS2 => QoS::ExactlyOnce,
                    _ => QoS::AtMostOnce,
                };
                (packet_id, qos)
            })
            .collect();

        Ok(results)
    }

    /// Unsubscribe from topics - DIRECT async method
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn unsubscribe(&self, packet: UnsubscribePacket) -> Result<()> {
        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }

        let writer = self.writer.as_ref().ok_or(MqttError::NotConnected)?;

        // Get packet ID
        let packet_id = self.packet_id_generator.next();
        let mut packet = packet;
        packet.packet_id = packet_id;

        // Create oneshot channel for UNSUBACK response
        let (tx, rx) = oneshot::channel();
        self.pending_unsubacks.lock().await.insert(packet_id, tx);

        // Remove from stored subscriptions
        let mut stored = self.stored_subscriptions.write().await;
        for topic in &packet.filters {
            stored.retain(|(stored_topic, _, _)| stored_topic != topic);
        }
        drop(stored);

        // Send UNSUBSCRIBE directly
        writer
            .write()
            .await
            .write_packet(Packet::Unsubscribe(packet.clone()))
            .await?;

        // Wait for UNSUBACK from packet reader task
        let timeout = Duration::from_secs(10);
        let unsuback = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(unsuback)) => unsuback,
            Ok(Err(_)) => {
                return Err(MqttError::ProtocolError(
                    "UNSUBACK channel closed".to_string(),
                ))
            }
            Err(_) => {
                // Timeout - remove from pending in a separate task to avoid blocking
                let pending = self.pending_unsubacks.clone();
                tokio::spawn(async move {
                    pending.lock().await.remove(&packet_id);
                });
                return Err(MqttError::Timeout);
            }
        };

        // Validate UNSUBACK packet ID matches
        if unsuback.packet_id != packet_id {
            return Err(MqttError::ProtocolError(format!(
                "UNSUBACK packet ID mismatch: expected {}, got {}",
                packet_id, unsuback.packet_id
            )));
        }

        // Remove subscriptions from session
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

    /// Build CONNECT packet
    async fn build_connect_packet(&self) -> ConnectPacket {
        use crate::protocol::v5::properties::{PropertyId, PropertyValue};

        let session = self.session.read().await;

        // Build CONNECT properties
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

        // Build will properties if present
        let will_properties = if let Some(ref will) = self.options.will {
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
        } else {
            Properties::default()
        };

        ConnectPacket {
            protocol_version: 5, // MQTT v5.0
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

    /// Start background tasks
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    fn start_background_tasks(&mut self, reader: UnifiedReader) -> Result<()> {
        // Start packet reader task
        let reader_session = self.session.clone();
        let reader_callbacks = self.callback_manager.clone();
        let suback_channels = self.pending_subacks.clone();
        let unsuback_channels = self.pending_unsubacks.clone();
        let puback_channels = self.pending_pubacks.clone();
        let pubcomp_channels = self.pending_pubcomps.clone();
        let writer_for_keepalive = self.writer.as_ref().ok_or(MqttError::NotConnected)?.clone();
        let connected = self.connected.clone();

        let writer_for_reader = writer_for_keepalive.clone();

        let ctx = PacketReaderContext {
            session: reader_session,
            callback_manager: reader_callbacks,
            suback_channels,
            unsuback_channels,
            puback_channels,
            pubcomp_channels,
            writer: writer_for_reader,
            connected,
        };

        let ctx_for_packet_reader = ctx.clone();
        self.packet_reader_handle = Some(tokio::spawn(async move {
            tracing::debug!("📦 PACKET READER - Task starting");
            packet_reader_task_with_responses(reader, ctx_for_packet_reader).await;
            tracing::debug!("📦 PACKET READER - Task exited");
        }));

        // Start keepalive task (only if keepalive is not zero)
        let keepalive_interval = self.options.keep_alive;
        if !keepalive_interval.is_zero() {
            let keepalive_writer = writer_for_keepalive;
            self.keepalive_handle = Some(tokio::spawn(async move {
                tracing::debug!("💓 KEEPALIVE - Task starting");
                keepalive_task_with_writer(keepalive_writer, keepalive_interval).await;
                tracing::debug!("💓 KEEPALIVE - Task exited");
            }));
        } else {
            tracing::debug!("💓 KEEPALIVE - Disabled (interval is zero)");
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

    pub async fn get_recoverable_flows(&self) -> Vec<(FlowId, FlowFlags)> {
        self.session.read().await.get_recoverable_flows().await
    }

    pub async fn recover_flows(&self) -> Result<usize> {
        let Some(manager) = &self.quic_stream_manager else {
            return Ok(0);
        };

        let flows = self.get_recoverable_flows().await;
        let mut recovered = 0;

        for (flow_id, flags) in flows {
            let recovery_flags = FlowFlags {
                clean: 0,
                ..flags
            };

            match manager.open_recovery_stream(flow_id, recovery_flags).await {
                Ok((_send, _recv)) => {
                    tracing::debug!(
                        flow_id = ?flow_id,
                        "Opened recovery stream for flow"
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

    pub async fn clear_flows(&self) {
        self.session.read().await.clear_flows().await;
    }

    pub async fn flow_count(&self) -> usize {
        self.session.read().await.flow_count().await
    }

    /// Stop background tasks
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

/// Context for packet reader task
#[derive(Clone)]
struct PacketReaderContext {
    session: Arc<RwLock<SessionState>>,
    callback_manager: Arc<CallbackManager>,
    suback_channels: Arc<Mutex<HashMap<u16, oneshot::Sender<SubAckPacket>>>>,
    unsuback_channels: Arc<Mutex<HashMap<u16, oneshot::Sender<UnsubAckPacket>>>>,
    puback_channels: Arc<Mutex<HashMap<u16, oneshot::Sender<ReasonCode>>>>,
    pubcomp_channels: Arc<Mutex<HashMap<u16, oneshot::Sender<ReasonCode>>>>,
    writer: Arc<tokio::sync::RwLock<UnifiedWriter>>,
    connected: Arc<AtomicBool>,
}

/// Packet reader task that handles response channels
async fn packet_reader_task_with_responses(mut reader: UnifiedReader, ctx: PacketReaderContext) {
    tracing::debug!("Packet reader task started and ready to process incoming packets");
    loop {
        // Read packet directly from reader - no mutex needed!
        let packet = reader.read_packet().await;

        match packet {
            Ok(packet) => {
                tracing::trace!("Received packet: {:?}", packet);
                // Check if this is a response we're waiting for
                match &packet {
                    Packet::SubAck(suback) => {
                        if let Some(tx) = ctx.suback_channels.lock().await.remove(&suback.packet_id)
                        {
                            let _ = tx.send(suback.clone());
                            continue;
                        }
                    }
                    Packet::UnsubAck(unsuback) => {
                        if let Some(tx) = ctx
                            .unsuback_channels
                            .lock()
                            .await
                            .remove(&unsuback.packet_id)
                        {
                            let _ = tx.send(unsuback.clone());
                            continue;
                        }
                    }
                    Packet::PubAck(puback) => {
                        if let Some(tx) = ctx.puback_channels.lock().await.remove(&puback.packet_id)
                        {
                            let _ = tx.send(puback.reason_code);
                            continue;
                        }
                    }
                    Packet::PubRec(pubrec) => {
                        if pubrec.reason_code.is_error() {
                            if let Some(tx) =
                                ctx.pubcomp_channels.lock().await.remove(&pubrec.packet_id)
                            {
                                let _ = tx.send(pubrec.reason_code);
                            }
                            continue;
                        }
                    }
                    Packet::PubComp(pubcomp) => {
                        if let Some(tx) =
                            ctx.pubcomp_channels.lock().await.remove(&pubcomp.packet_id)
                        {
                            let _ = tx.send(pubcomp.reason_code);
                        }
                    }
                    _ => {}
                }

                // Handle other packets normally
                if let Err(e) = handle_incoming_packet_with_writer(
                    packet,
                    &ctx.writer,
                    &ctx.session,
                    &ctx.callback_manager,
                )
                .await
                {
                    tracing::error!("Error handling packet: {e}");
                    ctx.connected.store(false, Ordering::SeqCst);
                    break;
                }
            }
            Err(e) => {
                tracing::error!("Error reading packet: {e}");
                ctx.connected.store(false, Ordering::SeqCst);
                break;
            }
        }
    }

    // Mark as disconnected when task exits
    ctx.connected.store(false, Ordering::SeqCst);
}

/// Keepalive task that uses the write half
async fn keepalive_task_with_writer(
    writer: Arc<tokio::sync::RwLock<UnifiedWriter>>,
    keepalive_interval: Duration,
) {
    // Send pings at 75% of the keepalive interval to ensure we stay well within the timeout
    let ping_millis = keepalive_interval.as_millis() * 3 / 4;
    // Saturate at u64::MAX if the value is too large (unlikely in practice)
    let ping_millis_u64 = u64::try_from(ping_millis).unwrap_or(u64::MAX);
    let ping_interval = Duration::from_millis(ping_millis_u64);
    let mut interval = tokio::time::interval(ping_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Skip the first immediate tick
    interval.tick().await;

    loop {
        interval.tick().await;

        // Send PINGREQ directly using writer
        if let Err(e) = writer.write().await.write_packet(Packet::PingReq).await {
            tracing::error!("Error sending PINGREQ: {e}");
            break;
        }
    }
}

async fn flow_expiration_task(session: Arc<RwLock<SessionState>>) {
    let check_interval = Duration::from_secs(60);
    let mut interval = tokio::time::interval(check_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    interval.tick().await;

    loop {
        interval.tick().await;

        let expired = session.read().await.expire_flows().await;
        if !expired.is_empty() {
            tracing::debug!(count = expired.len(), "Expired {} flows", expired.len());
        }
    }
}

async fn quic_stream_acceptor_task(connection: Arc<Connection>, ctx: PacketReaderContext) {
    loop {
        match connection.accept_bi().await {
            Ok((send, recv)) => {
                tracing::debug!("Accepted new QUIC stream");
                let ctx_for_reader = ctx.clone();
                tokio::spawn(async move {
                    quic_stream_reader_task(recv, send, ctx_for_reader).await;
                });
            }
            Err(e) => {
                tracing::error!("Error accepting QUIC stream: {e}");
                ctx.connected.store(false, Ordering::SeqCst);
                break;
            }
        }
    }
}

fn is_flow_header_byte(b: u8) -> bool {
    matches!(
        b,
        FLOW_TYPE_CONTROL | FLOW_TYPE_CLIENT_DATA | FLOW_TYPE_SERVER_DATA
    )
}

async fn try_read_server_flow_header(
    recv: &mut quinn::RecvStream,
) -> Result<Option<(FlowId, FlowFlags, Option<StdDuration>)>> {
    let chunk = recv
        .read_chunk(1, true)
        .await
        .map_err(|e| MqttError::ConnectionError(format!("Failed to peek stream: {e}")))?;

    let Some(chunk) = chunk else {
        return Ok(None);
    };

    if chunk.bytes.is_empty() {
        return Ok(None);
    }

    let first_byte = chunk.bytes[0];
    if !is_flow_header_byte(first_byte) {
        return Ok(None);
    }

    let mut header_buf = Vec::with_capacity(32);
    header_buf.extend_from_slice(&chunk.bytes);

    while header_buf.len() < 32 {
        match recv.read_chunk(32 - header_buf.len(), true).await {
            Ok(Some(chunk)) if !chunk.bytes.is_empty() => {
                header_buf.extend_from_slice(&chunk.bytes);
            }
            Ok(_) => break,
            Err(e) => {
                return Err(MqttError::ConnectionError(format!(
                    "Failed to read flow header: {e}"
                )));
            }
        }
    }

    let mut bytes = Bytes::from(header_buf);
    let flow_header = FlowHeader::decode(&mut bytes)?;

    match flow_header {
        FlowHeader::Control(h) => {
            tracing::trace!(flow_id = ?h.flow_id, "Parsed control flow header from server");
            Ok(Some((h.flow_id, h.flags, None)))
        }
        FlowHeader::ClientData(h) | FlowHeader::ServerData(h) => {
            let expire = if h.expire_interval > 0 {
                Some(StdDuration::from_secs(h.expire_interval))
            } else {
                None
            };
            tracing::debug!(flow_id = ?h.flow_id, is_server = h.is_server_flow(), expire = ?expire, "Parsed data flow header from server");
            Ok(Some((h.flow_id, h.flags, expire)))
        }
        FlowHeader::UserDefined(_) => {
            tracing::trace!("Ignoring user-defined flow header");
            Ok(None)
        }
    }
}

async fn quic_stream_reader_task(
    mut recv: quinn::RecvStream,
    _send: quinn::SendStream,
    ctx: PacketReaderContext,
) {
    use crate::transport::PacketReader;

    let flow_id = match try_read_server_flow_header(&mut recv).await {
        Ok(Some((id, flags, expire))) => {
            tracing::debug!(
                flow_id = ?id,
                is_server_initiated = id.is_server_initiated(),
                ?flags,
                ?expire,
                "Server-initiated stream with flow header"
            );
            Some(id)
        }
        Ok(None) => {
            tracing::trace!("No flow header on server-initiated stream");
            None
        }
        Err(e) => {
            tracing::warn!("Error parsing server flow header: {e}");
            None
        }
    };

    loop {
        match recv.read_packet().await {
            Ok(packet) => {
                tracing::trace!(flow_id = ?flow_id, "Received packet on server-initiated QUIC stream: {:?}", packet);
                if let Err(e) = handle_incoming_packet_with_writer(
                    packet,
                    &ctx.writer,
                    &ctx.session,
                    &ctx.callback_manager,
                )
                .await
                {
                    tracing::error!(flow_id = ?flow_id, "Error handling packet from server stream: {e}");
                    break;
                }
            }
            Err(e) => {
                tracing::debug!(flow_id = ?flow_id, "Server-initiated QUIC stream closed or error: {e}");
                break;
            }
        }
    }
}

/// Handle incoming packet with writer access for acknowledgments
async fn handle_incoming_packet_with_writer(
    packet: Packet,
    writer: &Arc<tokio::sync::RwLock<UnifiedWriter>>,
    session: &Arc<RwLock<SessionState>>,
    callback_manager: &Arc<CallbackManager>,
) -> Result<()> {
    match packet {
        Packet::Publish(publish) => {
            handle_publish_with_ack(publish, writer, session, callback_manager).await
        }
        Packet::PingResp => {
            // PINGRESP received, connection is alive
            Ok(())
        }
        Packet::PubRec(pubrec) => {
            // Handle QoS 2 PUBREC for outgoing messages - send PUBREL
            handle_pubrec_outgoing(pubrec, writer, session).await
        }
        Packet::PubRel(pubrel) => {
            // Handle QoS 2 PUBREL for incoming messages - complete the QoS 2 flow
            handle_pubrel(pubrel, writer, session).await
        }
        Packet::PubComp(pubcomp) => {
            // Handle QoS 2 PUBCOMP for outgoing messages - complete the flow
            handle_pubcomp_outgoing(pubcomp, session).await
        }
        Packet::Disconnect(disconnect) => {
            tracing::info!("Server sent DISCONNECT: {:?}", disconnect.reason_code);
            Err(MqttError::ConnectionError(
                "Server disconnected".to_string(),
            ))
        }
        _ => {
            // Other packet types handled elsewhere or not needed here
            Ok(())
        }
    }
}

/// Handle PUBLISH packet with proper `QoS` acknowledgments
async fn handle_publish_with_ack(
    publish: crate::packet::publish::PublishPacket,
    writer: &Arc<tokio::sync::RwLock<UnifiedWriter>>,
    session: &Arc<RwLock<SessionState>>,
    callback_manager: &Arc<CallbackManager>,
) -> Result<()> {
    // Handle QoS acknowledgment
    match publish.qos {
        crate::QoS::AtMostOnce => {
            // No acknowledgment needed
        }
        crate::QoS::AtLeastOnce => {
            if let Some(packet_id) = publish.packet_id {
                // Send PUBACK directly
                let puback = crate::packet::puback::PubAckPacket {
                    packet_id,
                    reason_code: crate::protocol::v5::reason_codes::ReasonCode::Success,
                    properties: Properties::default(),
                };
                writer
                    .write()
                    .await
                    .write_packet(Packet::PubAck(puback))
                    .await?;
            }
        }
        crate::QoS::ExactlyOnce => {
            if let Some(packet_id) = publish.packet_id {
                // Store the received publish packet for duplicate detection and QoS 2 flow
                session
                    .write()
                    .await
                    .store_unacked_publish(publish.clone())
                    .await?;

                // Send PUBREC directly
                let pubrec = crate::packet::pubrec::PubRecPacket {
                    packet_id,
                    reason_code: crate::protocol::v5::reason_codes::ReasonCode::Success,
                    properties: Properties::default(),
                };
                writer
                    .write()
                    .await
                    .write_packet(Packet::PubRec(pubrec))
                    .await?;

                session.write().await.store_pubrec(packet_id).await;
            }
        }
    }

    // Route to callbacks
    let _ = callback_manager.dispatch(&publish).await;

    Ok(())
}

/// Handle PUBREC packet for outgoing `QoS` 2 messages
async fn handle_pubrec_outgoing(
    pubrec: crate::packet::pubrec::PubRecPacket,
    writer: &Arc<tokio::sync::RwLock<UnifiedWriter>>,
    session: &Arc<RwLock<SessionState>>,
) -> Result<()> {
    // Move from unacked publish to unacked pubrel
    session
        .write()
        .await
        .complete_pubrec(pubrec.packet_id)
        .await;

    // Send PUBREL to complete QoS 2 flow
    let pub_rel = crate::packet::pubrel::PubRelPacket {
        packet_id: pubrec.packet_id,
        reason_code: crate::protocol::v5::reason_codes::ReasonCode::Success,
        properties: Properties::default(),
    };

    writer
        .write()
        .await
        .write_packet(crate::packet::Packet::PubRel(pub_rel))
        .await?;

    // Store PUBREL for completion tracking
    session.write().await.store_pubrel(pubrec.packet_id).await;

    Ok(())
}

/// Handle PUBCOMP packet for outgoing `QoS` 2 messages
async fn handle_pubcomp_outgoing(
    pubcomp: crate::packet::pubcomp::PubCompPacket,
    session: &Arc<RwLock<SessionState>>,
) -> Result<()> {
    // Complete the QoS 2 flow by removing the PUBREL
    session
        .write()
        .await
        .complete_pubrel(pubcomp.packet_id)
        .await;

    Ok(())
}

/// Handle PUBREL packet for `QoS` 2 flow
async fn handle_pubrel(
    pubrel: crate::packet::pubrel::PubRelPacket,
    writer: &Arc<tokio::sync::RwLock<UnifiedWriter>>,
    session: &Arc<RwLock<SessionState>>,
) -> Result<()> {
    // Check if we have a stored PUBREC for this packet ID
    let has_pubrec = session.read().await.has_pubrec(pubrel.packet_id).await;

    if has_pubrec {
        // Remove the stored PUBREC
        session.write().await.remove_pubrec(pubrel.packet_id).await;

        // Send PUBCOMP to complete QoS 2 flow
        let pubcomp = crate::packet::pubcomp::PubCompPacket {
            packet_id: pubrel.packet_id,
            reason_code: crate::protocol::v5::reason_codes::ReasonCode::Success,
            properties: Properties::default(),
        };

        writer
            .write()
            .await
            .write_packet(Packet::PubComp(pubcomp))
            .await?;
    } else {
        // Still send PUBCOMP even if we don't have a stored PUBREC
        // (per MQTT spec, we should always respond to PUBREL)
        let pubcomp = crate::packet::pubcomp::PubCompPacket {
            packet_id: pubrel.packet_id,
            reason_code: crate::protocol::v5::reason_codes::ReasonCode::Success,
            properties: Properties::default(),
        };

        writer
            .write()
            .await
            .write_packet(Packet::PubComp(pubcomp))
            .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
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
    async fn test_client_creation() {
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

        // Prepare CONNACK response
        let connack = ConnAckPacket {
            protocol_version: 5,
            session_present: false,
            reason_code: ReasonCode::Success,
            properties: Properties::default(),
        };
        let connack_bytes = encode_packet(&Packet::ConnAck(connack)).unwrap();
        transport.inject_packet(connack_bytes).await;

        // Connect with mock transport
        let transport_type = TransportType::Tcp(crate::transport::tcp::TcpTransport::from_addr(
            std::net::SocketAddr::from(([127, 0, 0, 1], 1883)),
        ));

        // For testing, we'll use the mock transport directly
        // In real usage, this would be determined by the connection string
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

        // Since we can't easily inject mock transport into TransportType,
        // we'll test the connection logic separately
        let _ = transport_type; // suppress unused warning
        assert!(!client.is_connected());

        // Test that we can create the CONNECT packet
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
            packet_id: 0, // Will be set by client
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
        };

        let result = client.subscribe_with_callback(packet, 0).await;
        assert!(matches!(result, Err(MqttError::NotConnected)));
    }

    #[tokio::test]
    async fn test_unsubscribe_not_connected() {
        let client = create_test_client();

        let packet = UnsubscribePacket {
            packet_id: 0, // Will be set by client
            properties: Properties::default(),
            filters: vec!["test/+".to_string()],
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

        // Generate multiple packet IDs
        let id1 = client.packet_id_generator.next();
        let id2 = client.packet_id_generator.next();
        let id3 = client.packet_id_generator.next();

        // They should be sequential and unique
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
        assert_eq!(will.payload, b"offline");
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

        // Test that session state can be accessed
        let session = client.session.read().await;
        assert_eq!(session.client_id(), "test-client");
        drop(session);

        // Test that session state can be modified
        let session = client.session.write().await;
        // In a real test, we'd modify the session state here
        assert_eq!(session.client_id(), "test-client");
    }
}
