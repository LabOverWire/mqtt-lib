use mqtt5::broker::auth::{EnhancedAuthResult, EnhancedAuthStatus};
use mqtt5::broker::router::DeliveryLanes;
use mqtt5::broker::storage::{ClientSession, StorageBackend};
use mqtt5_protocol::error::{MqttError, Result};
use mqtt5_protocol::packet::auth::AuthPacket;
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::{u64_to_u32_saturating, usize_to_u32_saturating};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tracing::{debug, error, info, warn};

use crate::decoder::read_packet;
use crate::transport::{WasmReader, WasmWriter};

use super::{AuthState, PendingConnect, WasmClientHandler};

impl WasmClientHandler {
    pub(super) async fn wait_for_connect(
        &mut self,
        reader: &mut WasmReader,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let packet = read_packet(reader).await?;

        let Packet::Connect(connect) = packet else {
            error!("First packet must be CONNECT");
            return Err(MqttError::ProtocolError(
                "First packet must be CONNECT".to_string(),
            ));
        };
        self.handle_connect(*connect, writer).await?;
        while self.pending_connect.is_some() {
            match read_packet(reader).await? {
                Packet::Auth(auth) => self.handle_auth(auth, writer).await?,
                _ => {
                    return Err(MqttError::ProtocolError(
                        "Only AUTH may follow CONNECT before CONNACK".to_string(),
                    ))
                }
            }
        }
        Ok(())
    }

    pub(super) async fn handle_connect(
        &mut self,
        mut connect: ConnectPacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        if connect.protocol_version != 4 && connect.protocol_version != 5 {
            let mut connack = ConnAckPacket::new(false, ReasonCode::UnsupportedProtocolVersion);
            connack.protocol_version = connect.protocol_version;
            self.write_packet(&Packet::ConnAck(connack), writer)?;
            return Err(MqttError::ProtocolError(
                "Unsupported protocol version".to_string(),
            ));
        }
        self.protocol_version = connect.protocol_version;

        self.check_load_balancer_redirect(&connect, writer)?;

        let mut assigned_client_id = None;
        if connect.client_id.is_empty() {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let generated_id = format!("wasm-auto-{}", COUNTER.fetch_add(1, Ordering::SeqCst));
            debug!("Generated client ID '{}' for empty client ID", generated_id);
            connect.client_id.clone_from(&generated_id);
            assigned_client_id = Some(generated_id);
        }

        if !mqtt5_protocol::is_path_safe_client_id(&connect.client_id) {
            let connack = ConnAckPacket::new(false, ReasonCode::ClientIdentifierNotValid);
            self.write_packet(&Packet::ConnAck(connack), writer)?;
            return Err(MqttError::InvalidClientId(connect.client_id));
        }

        let dummy_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

        if self.protocol_version == 5 {
            if let Some(auth_method) = connect.properties.get_authentication_method() {
                let auth_method = auth_method.clone();
                if self.auth_provider.supports_enhanced_auth() {
                    self.auth_method = Some(auth_method.clone());
                    self.auth_state = AuthState::InProgress;

                    let auth_data_owned = connect
                        .properties
                        .get_authentication_data()
                        .map(<[u8]>::to_vec);
                    let client_id_for_auth = connect.client_id.clone();
                    self.pending_connect = Some(PendingConnect {
                        connect,
                        assigned_client_id,
                    });

                    let result = self
                        .auth_provider
                        .authenticate_enhanced(
                            &auth_method,
                            auth_data_owned.as_deref(),
                            &client_id_for_auth,
                        )
                        .await?;

                    return self.process_enhanced_auth_result(result, writer).await;
                }
            }
        }

        let auth_result = self
            .auth_provider
            .authenticate(&connect, dummy_addr)
            .await?;

        if !auth_result.authenticated {
            let mut connack = ConnAckPacket::new(false, auth_result.reason_code);
            connack.protocol_version = self.protocol_version;
            self.write_packet(&Packet::ConnAck(connack), writer)?;
            return Err(MqttError::AuthenticationFailed);
        }

        self.validate_will_capabilities(&connect, writer)?;

        self.client_id = Some(connect.client_id.clone());
        self.user_id = auth_result.user_id;
        self.keep_alive = mqtt5::time::Duration::from_secs(u64::from(connect.keep_alive));
        self.auth_state = AuthState::Completed;

        let session_present = self.handle_session(&connect, writer).await?;

        let mut connack = ConnAckPacket::new(session_present, ReasonCode::Success);
        connack.protocol_version = self.protocol_version;

        if self.protocol_version == 5 {
            if let Some(ref assigned_id) = assigned_client_id {
                connack
                    .properties
                    .set_assigned_client_identifier(assigned_id.clone());
            }
            self.set_server_capability_properties(&mut connack);
        }

        self.write_packet(&Packet::ConnAck(connack), writer)?;

        self.fire_client_connect(&connect.client_id, connect.clean_start);

        if session_present {
            // Offline-queued messages are drained by the publish forwarder from the shared
            // per-client queue; delivering them here too would double-deliver.
            self.resend_inflight_messages(writer).await?;
            self.advance_packet_id_past_inflight();
        }
        Ok(())
    }

    pub(super) fn maximum_session_expiry(&self) -> u32 {
        self.config.read().map_or(u32::MAX, |config| {
            u64_to_u32_saturating(config.session_expiry_interval.as_secs())
        })
    }

    pub(super) async fn handle_session(
        &mut self,
        connect: &ConnectPacket,
        writer: &mut WasmWriter,
    ) -> Result<bool> {
        let client_id = connect.client_id.clone();
        let requested = ClientSession::expiry_from_connect(connect);
        let granted = ClientSession::granted_expiry(requested, self.maximum_session_expiry());
        self.connect_session_expiry = requested;
        self.advertised_session_expiry = (requested != Some(granted)).then_some(granted);

        let slot = self.router.lock_session(&client_id).await;
        let resumable = self
            .storage
            .get_session(&client_id)
            .await?
            .filter(|session| !connect.clean_start && session.expiry_interval != Some(0));
        if let Some(session) = resumable.as_ref() {
            if session.user_id.as_deref() != self.user_id.as_deref() {
                drop(slot);
                warn!(
                    client_id = %client_id,
                    session_user = ?session.user_id,
                    current_user = ?self.user_id,
                    "Session user mismatch, rejecting connection"
                );
                let connack = ConnAckPacket::new(false, ReasonCode::NotAuthorized);
                self.write_packet(&Packet::ConnAck(connack), writer)?;
                return Err(MqttError::AuthenticationFailed);
            }
        }
        let resume = resumable.is_some();
        let mut session = match resumable {
            Some(mut session) => {
                self.drop_unauthorized_subscriptions(&mut session).await;
                session.will_message.clone_from(&connect.will);
                session.will_delay_interval = connect
                    .will
                    .as_ref()
                    .and_then(|will| will.properties.will_delay_interval);
                session
            }
            None => ClientSession::new_with_will(
                client_id.clone(),
                granted != 0,
                Some(granted),
                connect.will.clone(),
            ),
        };
        session.expiry_interval = Some(granted);
        session.persistent = granted != 0;
        session.user_id.clone_from(&self.user_id);

        let generation = self.router.allocate_generation();
        session.mark_connected(generation);
        if let Err(e) = self.storage.store_session(session.clone()).await {
            drop(slot);
            warn!(client_id = %client_id, "Failed to store the claimed session: {e}");
            let mut connack = ConnAckPacket::new(false, ReasonCode::ServerUnavailable);
            connack.protocol_version = self.protocol_version;
            self.write_packet(&Packet::ConnAck(connack), writer)?;
            return Err(e);
        }

        let (disconnect_tx, disconnect_rx) = tokio::sync::oneshot::channel();
        let queue = self.router.queue_handle(&client_id);
        let registration = self
            .router
            .register_session_as(
                generation,
                client_id.clone(),
                DeliveryLanes {
                    qos1_tx: self.qos1_tx.clone(),
                    qos0_tx: self.qos0_tx.clone(),
                },
                queue.clone(),
                disconnect_tx,
                !resume,
            )
            .await;
        self.generation = generation;
        self.queue_epoch = registration.epoch;
        self.disconnect_rx = Some(disconnect_rx);
        self.queue = Some(queue);

        if !resume {
            if let Err(e) = self.storage.remove_all_inflight_messages(&client_id).await {
                warn!(client_id = %client_id, "Failed to discard inflight messages on clean start: {e}");
            }
        }
        if let Err(e) = self
            .router
            .set_client_subscriptions(&slot, Some(&session))
            .await
        {
            warn!(client_id = %client_id, "Failed to install session subscriptions: {e}");
        }
        drop(slot);

        self.session = Some(session);
        Ok(resume)
    }

    async fn drop_unauthorized_subscriptions(&self, session: &mut ClientSession) {
        let mut unauthorized = Vec::new();
        for topic_filter in session.subscriptions.keys() {
            let authorized = self
                .auth_provider
                .authorize_subscribe(&session.client_id, self.user_id.as_deref(), topic_filter)
                .await;
            if !authorized {
                unauthorized.push(topic_filter.clone());
            }
        }
        for filter in &unauthorized {
            session.subscriptions.remove(filter);
        }
    }

    pub(super) async fn process_enhanced_auth_result(
        &mut self,
        result: EnhancedAuthResult,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        match result.status {
            EnhancedAuthStatus::Success => {
                self.auth_state = AuthState::Completed;
                self.user_id.clone_from(&result.user_id);

                if let Some(pending) = self.pending_connect.take() {
                    self.validate_will_capabilities(&pending.connect, writer)?;
                    self.client_id = Some(pending.connect.client_id.clone());
                    self.keep_alive =
                        mqtt5::time::Duration::from_secs(u64::from(pending.connect.keep_alive));

                    let session_present = self.handle_session(&pending.connect, writer).await?;

                    let mut connack = ConnAckPacket::new(session_present, ReasonCode::Success);
                    connack.protocol_version = self.protocol_version;
                    if let Some(ref assigned_id) = pending.assigned_client_id {
                        connack
                            .properties
                            .set_assigned_client_identifier(assigned_id.clone());
                    }

                    connack
                        .properties
                        .set_authentication_method(result.auth_method);
                    if let Some(data) = result.auth_data {
                        connack.properties.set_authentication_data(data.into());
                    }

                    self.set_server_capability_properties(&mut connack);

                    self.write_packet(&Packet::ConnAck(connack), writer)?;

                    self.fire_client_connect(
                        &pending.connect.client_id,
                        pending.connect.clean_start,
                    );

                    if session_present {
                        // Offline-queued messages are drained by the publish forwarder; see
                        // handle_session above.
                        self.resend_inflight_messages(writer).await?;
                        self.advance_packet_id_past_inflight();
                    }
                }

                Ok(())
            }
            EnhancedAuthStatus::Continue => {
                let mut auth_packet = AuthPacket::new(ReasonCode::ContinueAuthentication);
                auth_packet
                    .properties
                    .set_authentication_method(result.auth_method);
                if let Some(data) = result.auth_data {
                    auth_packet.properties.set_authentication_data(data.into());
                }
                self.write_packet(&Packet::Auth(auth_packet), writer)?;
                Ok(())
            }
            EnhancedAuthStatus::Failed => {
                self.auth_state = AuthState::NotStarted;
                self.pending_connect = None;

                let mut connack = ConnAckPacket::new(false, result.reason_code);
                connack.protocol_version = self.protocol_version;
                self.write_packet(&Packet::ConnAck(connack), writer)?;
                Err(MqttError::AuthenticationFailed)
            }
        }
    }

    fn check_load_balancer_redirect(
        &self,
        connect: &ConnectPacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        if let Ok(config) = self.config.read() {
            if let Some(ref lb) = config.load_balancer {
                let generated;
                let client_id = if connect.client_id.is_empty() {
                    use std::sync::atomic::{AtomicU64, Ordering};
                    static LB_COUNTER: AtomicU64 = AtomicU64::new(0);
                    generated = format!("auto-{}", LB_COUNTER.fetch_add(1, Ordering::Relaxed));
                    &generated
                } else {
                    &connect.client_id
                };
                if let Some(backend) = lb.select_backend(client_id) {
                    let backend = backend.to_string();
                    info!(
                        client_id = %client_id,
                        backend = %backend,
                        "Redirecting client to backend"
                    );
                    let connack = ConnAckPacket::new(false, ReasonCode::UseAnotherServer)
                        .with_server_reference(backend);
                    self.write_packet(&Packet::ConnAck(connack), writer)?;
                    return Err(MqttError::UseAnotherServer);
                }
            }
        }
        Ok(())
    }

    fn validate_will_capabilities(
        &self,
        connect: &ConnectPacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        if let Some(ref will) = connect.will {
            if let Ok(config) = self.config.read() {
                if (will.qos as u8) > config.maximum_qos {
                    let mut connack = ConnAckPacket::new(false, ReasonCode::QoSNotSupported);
                    connack.protocol_version = self.protocol_version;
                    self.write_packet(&Packet::ConnAck(connack), writer)?;
                    return Err(MqttError::ProtocolError(
                        "Will QoS exceeds server maximum".to_string(),
                    ));
                }
                if will.retain && !config.retain_available {
                    let mut connack = ConnAckPacket::new(false, ReasonCode::RetainNotSupported);
                    connack.protocol_version = self.protocol_version;
                    self.write_packet(&Packet::ConnAck(connack), writer)?;
                    return Err(MqttError::ProtocolError("Retain not supported".to_string()));
                }
            }
        }
        Ok(())
    }

    fn set_server_capability_properties(&self, connack: &mut ConnAckPacket) {
        if let Some(granted) = self.advertised_session_expiry {
            connack.properties.set_session_expiry_interval(granted);
        }
        if let Ok(config) = self.config.read() {
            if config.maximum_qos < 2 {
                connack.properties.set_maximum_qos(config.maximum_qos);
            }
            connack
                .properties
                .set_retain_available(config.retain_available);
            connack
                .properties
                .set_maximum_packet_size(usize_to_u32_saturating(config.max_packet_size));
            connack
                .properties
                .set_topic_alias_maximum(config.topic_alias_maximum);
            connack
                .properties
                .set_wildcard_subscription_available(config.wildcard_subscription_available);
            connack
                .properties
                .set_subscription_identifier_available(config.subscription_identifier_available);
            connack
                .properties
                .set_shared_subscription_available(config.shared_subscription_available);
        }
    }
}
