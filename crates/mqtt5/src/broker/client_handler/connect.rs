use crate::broker::auth::EnhancedAuthStatus;
use crate::broker::router::DeliveryLanes;
use crate::broker::storage::{ClientSession, DynamicStorage, StorageBackend};
use crate::error::{MqttError, Result};
use crate::packet::auth::AuthPacket;
use crate::packet::connack::ConnAckPacket;
use crate::packet::connect::ConnectPacket;
use crate::packet::Packet;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::time::Duration;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tracing::{debug, info, trace, warn};

use super::{AuthState, ClientHandler, PendingConnect, SessionStart};

enum AuthOutcome {
    Authenticated(Box<ConnectPacket>),
    ConnectDeferred,
    Failed(MqttError),
}

fn clamp_keep_alive_to_u16(keep_alive: Duration) -> u16 {
    let secs = keep_alive.as_secs();
    u16::try_from(secs).unwrap_or_else(|_| {
        warn!(
            "Configured server_keep_alive {}s exceeds the u16 wire range; clamping to {}s",
            secs,
            u16::MAX,
        );
        u16::MAX
    })
}

impl ClientHandler {
    pub(super) async fn validate_protocol_version(&mut self, protocol_version: u8) -> Result<()> {
        match protocol_version {
            4 | 5 => {
                self.protocol_version = protocol_version;
                #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
                if let Some(tx) = &self.quic_protocol_version_tx {
                    tx.send_replace(Some(protocol_version));
                }
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
                self.write_to_client(Packet::ConnAck(connack)).await?;
                Err(MqttError::UnsupportedProtocolVersion)
            }
        }
    }

    pub(super) async fn handle_connect(
        &mut self,
        mut connect: ConnectPacket,
    ) -> Result<Option<PendingConnect>> {
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

        if let Some(redirect) = self.check_load_balancer_redirect(&connect).await? {
            return redirect.map(|()| None);
        }

        self.request_problem_information = connect
            .properties
            .get_request_problem_information()
            .unwrap_or(true);
        self.request_response_information = connect
            .properties
            .get_request_response_information()
            .unwrap_or(false);
        let client_max_packet_size = connect.properties.get_maximum_packet_size();
        if client_max_packet_size == Some(0) {
            warn!(
                addr = %self.client_addr,
                "Rejecting connection: Maximum Packet Size of 0 is a Protocol Error"
            );
            let connack = ConnAckPacket::new(false, ReasonCode::ProtocolError);
            self.write_to_client(Packet::ConnAck(connack)).await?;
            return Err(MqttError::ProtocolError(
                "Maximum Packet Size must not be zero".to_string(),
            ));
        }
        self.client_max_packet_size = client_max_packet_size;

        let assigned_client_id = Self::assign_client_id_if_empty(&mut connect);
        self.validate_client_id(&connect).await?;

        match self
            .handle_authentication(connect, assigned_client_id.clone())
            .await?
        {
            AuthOutcome::Authenticated(connect) => Ok(Some(PendingConnect {
                connect: *connect,
                assigned_client_id,
            })),
            AuthOutcome::ConnectDeferred => Ok(None),
            AuthOutcome::Failed(err) => Err(err),
        }
    }

    pub(super) async fn complete_connect(&mut self, accepted: PendingConnect) -> Result<()> {
        let PendingConnect {
            connect,
            assigned_client_id,
        } = accepted;
        self.validate_will_capabilities(&connect).await?;

        self.client_id = Some(connect.client_id.clone());
        self.keep_alive = Duration::from_secs(u64::from(connect.keep_alive));

        self.client_receive_maximum = connect.properties.get_receive_maximum().unwrap_or(65535);
        debug!(
            client_id = %connect.client_id,
            receive_maximum = self.client_receive_maximum,
            max_packet_size = ?self.client_max_packet_size,
            "Client connection limits"
        );

        #[cfg(feature = "opentelemetry")]
        let session_present = {
            use tracing::Instrument;
            let span = tracing::info_span!(
                "mqtt.session",
                mqtt.client_id = %connect.client_id,
            );
            self.handle_session(&connect).instrument(span).await?
        };
        #[cfg(not(feature = "opentelemetry"))]
        let session_present = self.handle_session(&connect).await?;

        let mut connack = self.new_connack(session_present, ReasonCode::Success);
        if self.protocol_version == 5 {
            self.build_connack_properties(&mut connack, assigned_client_id.as_ref());
            if let Some(method) = self.auth_method.clone() {
                connack.properties.set_authentication_method(method);
                if let Some(data) = self.connack_auth_data.take() {
                    connack.properties.set_authentication_data(data.into());
                }
            }
        }

        debug!(
            client_id = %connect.client_id,
            session_present = session_present,
            assigned_client_id = ?assigned_client_id,
            "Sending CONNACK"
        );
        trace!("CONNACK properties: {:?}", connack.properties);
        self.write_to_client(Packet::ConnAck(connack)).await?;
        debug!("CONNACK sent successfully");

        Ok(())
    }

    async fn check_load_balancer_redirect(
        &mut self,
        connect: &ConnectPacket,
    ) -> Result<Option<Result<()>>> {
        let Some(ref lb) = self.config.load_balancer else {
            return Ok(None);
        };
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
                addr = %self.client_addr,
                "Redirecting client to backend"
            );
            let connack = ConnAckPacket::new(false, ReasonCode::UseAnotherServer)
                .with_server_reference(backend);
            self.write_to_client(Packet::ConnAck(connack)).await?;
            return Ok(Some(Err(MqttError::UseAnotherServer)));
        }
        Ok(None)
    }

    fn assign_client_id_if_empty(connect: &mut ConnectPacket) -> Option<String> {
        if connect.client_id.is_empty() {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let generated_id = format!("auto-{}", COUNTER.fetch_add(1, Ordering::SeqCst));
            debug!("Generated client ID '{}' for empty client ID", generated_id);
            connect.client_id.clone_from(&generated_id);
            Some(generated_id)
        } else {
            None
        }
    }

    async fn validate_client_id(&mut self, connect: &ConnectPacket) -> Result<()> {
        if !crate::is_path_safe_client_id(&connect.client_id) {
            warn!(
                client_id = %connect.client_id,
                addr = %self.client_addr,
                "Rejecting connection: invalid client identifier"
            );
            let connack = ConnAckPacket::new(false, ReasonCode::ClientIdentifierNotValid);
            self.write_to_client(Packet::ConnAck(connack)).await?;
            return Err(MqttError::InvalidClientId(connect.client_id.clone()));
        }

        if connect.client_id.starts_with("cert:") && self.transport.client_cert_info().is_none() {
            warn!(
                client_id = %connect.client_id,
                transport = self.transport.transport_type(),
                addr = %self.client_addr,
                "Rejecting cert: client ID on connection without verified client certificate"
            );
            let connack = ConnAckPacket::new(false, ReasonCode::NotAuthorized);
            self.write_to_client(Packet::ConnAck(connack)).await?;
            return Err(MqttError::AuthenticationFailed);
        }

        Ok(())
    }

    async fn handle_authentication(
        &mut self,
        connect: ConnectPacket,
        assigned_client_id: Option<String>,
    ) -> Result<AuthOutcome> {
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
                        self.user_id = result.user_id;
                        self.connack_auth_data = result.auth_data;
                    }
                    EnhancedAuthStatus::Continue => {
                        self.auth_state = AuthState::InProgress;
                        self.keep_alive = Duration::from_secs(u64::from(connect.keep_alive));

                        let auth_packet = AuthPacket::continue_authentication(
                            result.auth_method,
                            result.auth_data,
                        )?;
                        self.write_to_client(Packet::Auth(auth_packet)).await?;

                        self.pending_connect = Some(PendingConnect {
                            connect,
                            assigned_client_id,
                        });

                        return Ok(AuthOutcome::ConnectDeferred);
                    }
                    EnhancedAuthStatus::Failed => {
                        let mut connack = self.new_connack(false, result.reason_code);
                        if self.protocol_version == 5 && self.request_problem_information {
                            if let Some(reason) = result.reason_string {
                                connack.properties.set_reason_string(reason);
                            }
                        }
                        self.write_to_client(Packet::ConnAck(connack)).await?;
                        return Ok(AuthOutcome::Failed(MqttError::AuthenticationFailed));
                    }
                }
            } else {
                let mut connack = self.new_connack(false, ReasonCode::BadAuthenticationMethod);
                if self.protocol_version == 5 && self.request_problem_information {
                    connack.properties.set_reason_string(
                        "Server does not support enhanced authentication".to_string(),
                    );
                }
                self.write_to_client(Packet::ConnAck(connack)).await?;
                return Ok(AuthOutcome::Failed(MqttError::AuthenticationFailed));
            }
        } else {
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
                let mut connack = self.new_connack(false, auth_result.reason_code);
                if self.protocol_version == 5 && self.request_problem_information {
                    connack
                        .properties
                        .set_reason_string("Authentication failed".to_string());
                }
                self.write_to_client(Packet::ConnAck(connack)).await?;
                return Ok(AuthOutcome::Failed(MqttError::AuthenticationFailed));
            }

            self.user_id = auth_result.user_id;
        }

        Ok(AuthOutcome::Authenticated(Box::new(connect)))
    }

    pub(super) fn new_connack(
        &self,
        session_present: bool,
        reason_code: ReasonCode,
    ) -> ConnAckPacket {
        if self.protocol_version == 4 {
            ConnAckPacket::new_v311(session_present, reason_code)
        } else {
            ConnAckPacket::new(session_present, reason_code)
        }
    }

    async fn validate_will_capabilities(&mut self, connect: &ConnectPacket) -> Result<()> {
        if let Some(ref will) = connect.will {
            if (will.qos as u8) > self.config.maximum_qos {
                info!(
                    client_id = %connect.client_id,
                    will_qos = will.qos as u8,
                    maximum_qos = self.config.maximum_qos,
                    "Rejecting connection: Will QoS exceeds server maximum"
                );
                let connack = self.new_connack(false, ReasonCode::QoSNotSupported);
                self.write_to_client(Packet::ConnAck(connack)).await?;
                return Err(MqttError::ProtocolError(
                    "Will QoS exceeds server maximum".into(),
                ));
            }
            if will.retain && !self.config.retain_available {
                info!(
                    client_id = %connect.client_id,
                    "Rejecting connection: Will Retain not supported"
                );
                let connack = self.new_connack(false, ReasonCode::RetainNotSupported);
                self.write_to_client(Packet::ConnAck(connack)).await?;
                return Err(MqttError::ProtocolError("Retain not supported".into()));
            }
        }
        Ok(())
    }

    pub(super) fn build_connack_properties(
        &mut self,
        connack: &mut ConnAckPacket,
        assigned_client_id: Option<&String>,
    ) {
        if let Some(assigned_id) = assigned_client_id {
            debug!("Setting assigned client ID in CONNACK: {}", assigned_id);
            connack
                .properties
                .set_assigned_client_identifier(assigned_id.clone());
        }

        if let Some(granted) = self.advertised_session_expiry {
            connack.properties.set_session_expiry_interval(granted);
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
        connack
            .properties
            .set_subscription_identifier_available(self.config.subscription_identifier_available);
        connack
            .properties
            .set_shared_subscription_available(self.config.shared_subscription_available);

        if self.config.maximum_qos < 2 {
            connack.properties.set_maximum_qos(self.config.maximum_qos);
        }

        if let Some(recv_max) = self.config.server_receive_maximum {
            connack.properties.set_receive_maximum(recv_max);
        }

        if let Some(keep_alive) = self.config.server_keep_alive {
            let secs = clamp_keep_alive_to_u16(keep_alive);
            connack.properties.set_server_keep_alive(secs);
            self.keep_alive = Duration::from_secs(u64::from(secs));
            debug!(
                client_id = ?self.client_id,
                negotiated_keep_alive_secs = secs,
                "Broker overrode client keep-alive via ServerKeepAlive"
            );
        }

        if self.request_response_information {
            if let Some(ref response_info) = self.config.response_information {
                connack
                    .properties
                    .set_response_information(response_info.clone());
            }
        }
    }

    pub(super) fn maximum_session_expiry(&self) -> u32 {
        if self.storage.is_none() {
            return 0;
        }
        u32::try_from(self.config.session_expiry_interval.as_secs()).unwrap_or(u32::MAX)
    }

    pub(super) async fn handle_session(&mut self, connect: &ConnectPacket) -> Result<bool> {
        let client_id = connect.client_id.clone();
        let requested = ClientSession::expiry_from_connect(connect);
        let granted = ClientSession::granted_expiry(requested, self.maximum_session_expiry());
        self.connect_session_expiry = requested;
        self.advertised_session_expiry = (requested != Some(granted)).then_some(granted);

        let slot = self.router.lock_session(&client_id).await;
        let stored = match self.storage.as_ref() {
            Some(storage) => storage.get_session(&client_id).await?,
            None => None,
        };
        let resumable =
            stored.filter(|session| !connect.clean_start && session.expiry_interval != Some(0));
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
                self.write_to_client(Packet::ConnAck(connack)).await?;
                return Err(MqttError::AuthenticationFailed);
            }
        }
        let resume = resumable.is_some();
        let mut session = self.session_for_claim(connect, resumable, granted).await;

        let generation = self.router.allocate_generation();
        session.mark_connected(generation);
        if let Some(storage) = self.storage.clone() {
            if let Err(e) = Self::write_claim(&storage, &session, resume).await {
                drop(slot);
                warn!(client_id = %client_id, "Failed to store the claimed session: {e}");
                let connack = self.new_connack(false, ReasonCode::ServerUnavailable);
                self.write_to_client(Packet::ConnAck(connack)).await?;
                return Err(e);
            }
        }

        let (disconnect_tx, disconnect_rx) = oneshot::channel();
        let queue = self.router.queue_handle(&client_id);
        let registration = self
            .router
            .register_unbound_session(
                generation,
                client_id.clone(),
                DeliveryLanes {
                    qos1_tx: self.qos1_tx.clone(),
                    qos0_tx: self.qos0_tx.clone(),
                },
                Arc::clone(&queue),
                disconnect_tx,
                !resume,
            )
            .await;
        self.generation = generation;
        self.queue_epoch = registration.epoch;
        self.handoff_deadline = registration
            .released
            .as_ref()
            .map(|_| Instant::now() + super::HANDOFF_BOUND);
        self.released_rx = registration.released;
        self.disconnect_rx = Some(disconnect_rx);
        self.queue = Some(queue);
        self.session_start = SessionStart::of(connect.clean_start, resume);

        if let Err(e) = self
            .router
            .set_client_subscriptions(&slot, Some(&session))
            .await
        {
            warn!(client_id = %client_id, "Failed to install session subscriptions: {e}");
        }
        if resume {
            self.router
                .load_change_only_state(&client_id, session.change_only_state.clone())
                .await;
        }
        drop(slot);

        debug!(
            client_id = %client_id,
            generation,
            session_present = resume,
            session_expiry = granted,
            "Session claimed"
        );
        self.session = Some(session);
        Ok(resume)
    }

    async fn session_for_claim(
        &self,
        connect: &ConnectPacket,
        resumable: Option<ClientSession>,
        granted: u32,
    ) -> ClientSession {
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
                connect.client_id.clone(),
                true,
                Some(granted),
                connect.will.clone(),
            ),
        };
        session.expiry_interval = Some(granted);
        session.receive_maximum = self.client_receive_maximum;
        session.user_id.clone_from(&self.user_id);
        session
    }

    async fn write_claim(
        storage: &DynamicStorage,
        session: &ClientSession,
        resume: bool,
    ) -> Result<()> {
        storage.store_session(session.clone()).await?;
        if !resume {
            if let Err(e) = storage
                .remove_all_inflight_messages(&session.client_id)
                .await
            {
                warn!(
                    client_id = %session.client_id,
                    "Failed to discard the inflight messages of the replaced session: {e}"
                );
            }
        }
        Ok(())
    }

    async fn drop_unauthorized_subscriptions(&self, session: &mut ClientSession) {
        let mut unauthorized = Vec::new();
        for topic_filter in session.subscriptions.keys() {
            let authorized = self
                .auth_provider
                .authorize_subscribe(&session.client_id, self.user_id.as_deref(), topic_filter)
                .await;
            if !authorized {
                warn!(
                    client_id = %session.client_id,
                    topic_filter = %topic_filter,
                    "Dropping subscription on session restore: no longer authorized"
                );
                unauthorized.push(topic_filter.clone());
            }
        }
        for filter in &unauthorized {
            session.subscriptions.remove(filter);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ClientHandler, SessionStart};
    use crate::broker::auth::AllowAllAuthProvider;
    use crate::broker::config::BrokerConfig;
    use crate::broker::resource_monitor::{ResourceLimits, ResourceMonitor};
    use crate::broker::router::{DeliveryLanes, MessageRouter};
    use crate::broker::storage::{DynamicStorage, MemoryBackend};
    use crate::broker::sys_topics::BrokerStats;
    use crate::broker::transport::BrokerTransport;
    use crate::packet::connect::ConnectPacket;
    use crate::time::Duration;
    use std::sync::Arc;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::{broadcast, mpsc, oneshot};

    #[tokio::test]
    async fn accepted_session_cancels_pending_will_before_connack() {
        let storage = Arc::new(DynamicStorage::Memory(MemoryBackend::new()));
        let router = Arc::new(MessageRouter::with_storage(Arc::clone(&storage)));

        let (qos1_tx, _qos1_rx) = mpsc::channel(4);
        let (qos0_tx, _qos0_rx) = mpsc::channel(4);
        let (disconnect_tx, _disconnect_rx) = oneshot::channel();
        let departing = router
            .register_client(
                "early-cancel".to_string(),
                DeliveryLanes { qos1_tx, qos0_tx },
                router.queue_handle("early-cancel"),
                disconnect_tx,
            )
            .await
            .generation;
        let cancelled = router
            .arm_will("early-cancel", departing)
            .await
            .expect("the departing connection owns its entry");
        router.release_client("early-cancel", departing, true).await;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let _client = TcpStream::connect(addr).await.expect("connect");
        let (server, peer) = listener.accept().await.expect("accept");
        let (_shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let mut handler = ClientHandler::new(
            BrokerTransport::tcp(server),
            peer,
            Arc::new(BrokerConfig::default()),
            Arc::clone(&router),
            Arc::new(AllowAllAuthProvider),
            Some(storage),
            Arc::new(BrokerStats::new()),
            Arc::new(ResourceMonitor::new(ResourceLimits::default())),
            shutdown_rx,
        );

        let connect = ConnectPacket::new(
            crate::types::ConnectOptions::new("early-cancel")
                .with_clean_start(false)
                .with_session_expiry_interval(60)
                .protocol_options,
        );
        handler
            .handle_session(&connect)
            .await
            .expect("session accepted");

        let woken = tokio::time::timeout(Duration::from_secs(1), cancelled).await;
        assert!(
            matches!(woken, Ok(Ok(()))),
            "an accepted session must cancel the pending Will before CONNACK, not only at registration"
        );
    }

    async fn claim_with_inflight(clean_start: bool) -> usize {
        use crate::broker::storage::{
            ClientSession, InflightDirection, InflightMessage, InflightPhase, StorageBackend,
        };
        let storage = Arc::new(DynamicStorage::Memory(MemoryBackend::new()));
        let mut stored = ClientSession::new("held-inflight", true, Some(60));
        stored.mark_disconnected(crate::broker::storage::unix_millis_now());
        storage.store_session(stored).await.expect("store session");
        let mut publish = crate::packet::publish::PublishPacket::new(
            "inflight/t".to_string(),
            b"unacked".to_vec(),
            crate::QoS::AtLeastOnce,
        );
        publish.packet_id = Some(7);
        storage
            .store_inflight_message(InflightMessage::from_publish(
                &publish,
                "held-inflight".to_string(),
                InflightDirection::Outbound,
                InflightPhase::AwaitingPubrec,
            ))
            .await
            .expect("store inflight");
        let router = Arc::new(MessageRouter::with_storage(Arc::clone(&storage)));

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let _client = TcpStream::connect(addr).await.expect("connect");
        let (server, peer) = listener.accept().await.expect("accept");
        let (_shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let mut handler = ClientHandler::new(
            BrokerTransport::tcp(server),
            peer,
            Arc::new(BrokerConfig::default()),
            router,
            Arc::new(AllowAllAuthProvider),
            Some(Arc::clone(&storage)),
            Arc::new(BrokerStats::new()),
            Arc::new(ResourceMonitor::new(ResourceLimits::default())),
            shutdown_rx,
        );
        let connect = ConnectPacket::new(
            crate::types::ConnectOptions::new("held-inflight")
                .with_clean_start(clean_start)
                .with_session_expiry_interval(60)
                .protocol_options,
        );
        let present = handler.handle_session(&connect).await.expect("claim");
        assert_eq!(present, !clean_start);
        storage
            .get_inflight_messages("held-inflight")
            .await
            .expect("read inflight")
            .len()
    }

    #[tokio::test]
    async fn resuming_claim_keeps_the_persisted_inflight_messages() {
        assert_eq!(
            claim_with_inflight(false).await,
            1,
            "a resumed session lost its unacknowledged messages at the claim"
        );
    }

    #[tokio::test]
    async fn clean_start_claim_discards_the_persisted_inflight_messages() {
        assert_eq!(
            claim_with_inflight(true).await,
            0,
            "a clean start kept the old session's unacknowledged messages past its claim"
        );
    }

    #[test]
    fn session_start_keeps_the_client_flag_apart_from_the_fresh_session_decision() {
        let cases = [
            (true, true, SessionStart::CleanStart, true, true),
            (true, false, SessionStart::CleanStart, true, true),
            (false, true, SessionStart::Resumed, false, false),
            (false, false, SessionStart::NothingToResume, true, false),
        ];
        for (clean_start, resumed, expected, fresh, reported) in cases {
            let start = SessionStart::of(clean_start, resumed);
            assert_eq!(
                start, expected,
                "clean_start={clean_start} resumed={resumed}"
            );
            assert_eq!(start.is_fresh(), fresh, "{start:?} is_fresh");
            assert_eq!(
                start.clean_start_requested(),
                reported,
                "{start:?} clean_start_requested"
            );
        }
    }
}
