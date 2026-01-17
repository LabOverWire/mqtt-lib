use mqtt5::broker::auth::{EnhancedAuthResult, EnhancedAuthStatus};
use mqtt5::broker::storage::{ClientSession, StorageBackend};
use mqtt5_protocol::error::{MqttError, Result};
use mqtt5_protocol::packet::auth::AuthPacket;
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::types::ProtocolVersion;
use mqtt5_protocol::{u64_to_u32_saturating, usize_to_u32_saturating};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tracing::{debug, error};

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

        if let Packet::Connect(connect) = packet {
            self.handle_connect(*connect, writer).await
        } else {
            error!("First packet must be CONNECT");
            Err(MqttError::ProtocolError(
                "First packet must be CONNECT".to_string(),
            ))
        }
    }

    #[allow(clippy::too_many_lines)]
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

        let mut assigned_client_id = None;
        if connect.client_id.is_empty() {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let generated_id = format!("wasm-auto-{}", COUNTER.fetch_add(1, Ordering::SeqCst));
            debug!("Generated client ID '{}' for empty client ID", generated_id);
            connect.client_id.clone_from(&generated_id);
            assigned_client_id = Some(generated_id);
        }

        let dummy_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

        if self.protocol_version == 5 {
            if let Some(auth_method) = connect.properties.get_authentication_method() {
                if self.auth_provider.supports_enhanced_auth() {
                    self.auth_method = Some(auth_method.clone());
                    self.auth_state = AuthState::InProgress;
                    self.pending_connect = Some(PendingConnect {
                        connect: connect.clone(),
                        assigned_client_id: assigned_client_id.clone(),
                    });

                    let auth_data = connect.properties.get_authentication_data();
                    let result = self
                        .auth_provider
                        .authenticate_enhanced(auth_method, auth_data, &connect.client_id)
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

        self.client_id = Some(connect.client_id.clone());
        self.user_id = auth_result.user_id;
        self.keep_alive = mqtt5::time::Duration::from_secs(u64::from(connect.keep_alive));
        self.auth_state = AuthState::Completed;

        let session_present = self.handle_session(&connect).await?;

        let mut connack = ConnAckPacket::new(session_present, ReasonCode::Success);
        connack.protocol_version = self.protocol_version;

        if self.protocol_version == 5 {
            if let Some(ref assigned_id) = assigned_client_id {
                connack
                    .properties
                    .set_assigned_client_identifier(assigned_id.clone());
            }

            if let Ok(config) = self.config.read() {
                connack
                    .properties
                    .set_session_expiry_interval(u64_to_u32_saturating(
                        config.session_expiry_interval.as_secs(),
                    ));
                if config.maximum_qos < 2 {
                    connack.properties.set_maximum_qos(config.maximum_qos);
                }
                connack.properties.set_retain_available(true);
                connack
                    .properties
                    .set_maximum_packet_size(usize_to_u32_saturating(config.max_packet_size));
                connack
                    .properties
                    .set_topic_alias_maximum(config.topic_alias_maximum);
                connack
                    .properties
                    .set_wildcard_subscription_available(config.wildcard_subscription_available);
                connack.properties.set_subscription_identifier_available(
                    config.subscription_identifier_available,
                );
                connack
                    .properties
                    .set_shared_subscription_available(config.shared_subscription_available);
            }
        }

        self.write_packet(&Packet::ConnAck(connack), writer)?;

        self.fire_client_connect(&connect.client_id, connect.clean_start);

        if session_present {
            self.deliver_queued_messages(&connect.client_id, writer)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn handle_session(&mut self, connect: &ConnectPacket) -> Result<bool> {
        let client_id = &connect.client_id;
        let session_expiry = connect.properties.get_session_expiry_interval();

        if connect.clean_start {
            self.storage.remove_session(client_id).await.ok();
            self.storage.remove_queued_messages(client_id).await.ok();

            let session = ClientSession::new_with_will(
                client_id.clone(),
                session_expiry != Some(0),
                session_expiry,
                connect.will.clone(),
            );
            self.storage.store_session(session.clone()).await.ok();
            self.session = Some(session);
            Ok(false)
        } else {
            match self.storage.get_session(client_id).await {
                Ok(Some(mut session)) => {
                    for (topic_filter, stored) in &session.subscriptions {
                        self.router
                            .subscribe(
                                client_id.clone(),
                                topic_filter.clone(),
                                stored.qos,
                                stored.subscription_id,
                                stored.no_local,
                                stored.retain_as_published,
                                stored.retain_handling,
                                ProtocolVersion::try_from(self.protocol_version)
                                    .unwrap_or_default(),
                            )
                            .await?;
                    }

                    session.will_message.clone_from(&connect.will);
                    session.will_delay_interval = connect
                        .will
                        .as_ref()
                        .and_then(|w| w.properties.will_delay_interval);
                    self.storage.store_session(session.clone()).await.ok();
                    self.session = Some(session);
                    Ok(true)
                }
                Ok(None) => {
                    let session = ClientSession::new_with_will(
                        client_id.clone(),
                        session_expiry != Some(0),
                        session_expiry,
                        connect.will.clone(),
                    );
                    self.storage.store_session(session.clone()).await.ok();
                    self.session = Some(session);
                    Ok(false)
                }
                Err(e) => Err(e),
            }
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

                if let Some(pending) = self.pending_connect.take() {
                    self.client_id = Some(pending.connect.client_id.clone());
                    self.keep_alive =
                        mqtt5::time::Duration::from_secs(u64::from(pending.connect.keep_alive));

                    let session_present = self.handle_session(&pending.connect).await?;

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

                    if let Ok(config) = self.config.read() {
                        connack
                            .properties
                            .set_session_expiry_interval(u64_to_u32_saturating(
                                config.session_expiry_interval.as_secs(),
                            ));
                        if config.maximum_qos < 2 {
                            connack.properties.set_maximum_qos(config.maximum_qos);
                        }
                        connack.properties.set_retain_available(true);
                        connack
                            .properties
                            .set_maximum_packet_size(usize_to_u32_saturating(
                                config.max_packet_size,
                            ));
                        connack
                            .properties
                            .set_topic_alias_maximum(config.topic_alias_maximum);
                        connack.properties.set_wildcard_subscription_available(
                            config.wildcard_subscription_available,
                        );
                        connack.properties.set_subscription_identifier_available(
                            config.subscription_identifier_available,
                        );
                        connack.properties.set_shared_subscription_available(
                            config.shared_subscription_available,
                        );
                    }

                    self.write_packet(&Packet::ConnAck(connack), writer)?;

                    self.fire_client_connect(
                        &pending.connect.client_id,
                        pending.connect.clean_start,
                    );

                    if session_present {
                        self.deliver_queued_messages(&pending.connect.client_id, writer)
                            .await?;
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
}
