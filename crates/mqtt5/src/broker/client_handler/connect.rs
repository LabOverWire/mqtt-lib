//! Connection handling and session management

use crate::broker::auth::EnhancedAuthStatus;
use crate::broker::storage::{ClientSession, DynamicStorage, StorageBackend};
use crate::error::{MqttError, Result};
use crate::packet::auth::AuthPacket;
use crate::packet::connack::ConnAckPacket;
use crate::packet::connect::ConnectPacket;
use crate::packet::Packet;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::time::Duration;
use crate::transport::PacketIo;
use crate::types::ProtocolVersion;
use crate::QoS;
use std::sync::Arc;
use tracing::{debug, info, warn};

use super::{AuthState, ClientHandler, PendingConnect};

impl ClientHandler {
    pub(super) async fn validate_protocol_version(&mut self, protocol_version: u8) -> Result<()> {
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

    #[allow(clippy::too_many_lines)]
    pub(super) async fn handle_connect(&mut self, mut connect: ConnectPacket) -> Result<()> {
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

        let mut assigned_client_id = None;
        if connect.client_id.is_empty() {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let generated_id = format!("auto-{}", COUNTER.fetch_add(1, Ordering::SeqCst));
            debug!("Generated client ID '{}' for empty client ID", generated_id);
            connect.client_id.clone_from(&generated_id);
            assigned_client_id = Some(generated_id.clone());
        }

        if !crate::is_path_safe_client_id(&connect.client_id) {
            warn!(
                client_id = %connect.client_id,
                addr = %self.client_addr,
                "Rejecting connection: invalid client identifier"
            );
            let connack = ConnAckPacket::new(false, ReasonCode::ClientIdentifierNotValid);
            self.transport
                .write_packet(Packet::ConnAck(connack))
                .await?;
            return Err(MqttError::InvalidClientId(connect.client_id));
        }

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

        self.client_id = Some(connect.client_id.clone());
        self.keep_alive = Duration::from_secs(u64::from(connect.keep_alive));

        self.client_receive_maximum = connect.properties.get_receive_maximum().unwrap_or(65535);
        debug!(
            client_id = %connect.client_id,
            receive_maximum = self.client_receive_maximum,
            "Client receive maximum"
        );

        let session_present = self.handle_session(&connect).await?;

        let mut connack = if self.protocol_version == 4 {
            ConnAckPacket::new_v311(session_present, ReasonCode::Success)
        } else {
            ConnAckPacket::new(session_present, ReasonCode::Success)
        };

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

        if session_present {
            self.deliver_queued_messages(&connect.client_id).await?;
            self.resend_inflight_messages().await?;
        }

        Ok(())
    }

    pub(super) async fn handle_session(&mut self, connect: &ConnectPacket) -> Result<bool> {
        let mut session_present = false;
        if let Some(storage) = self.storage.clone() {
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

    async fn create_new_session(
        &mut self,
        connect: &ConnectPacket,
        storage: &Arc<DynamicStorage>,
    ) -> Result<()> {
        let session_expiry = connect.properties.get_session_expiry_interval();

        let will_message = connect.will.clone();
        if let Some(ref will) = will_message {
            debug!(
                "Will message present with delay: {:?}",
                will.properties.will_delay_interval
            );
        }

        let mut session = ClientSession::new_with_will(
            connect.client_id.clone(),
            true,
            session_expiry,
            will_message,
        );
        session.receive_maximum = self.client_receive_maximum;
        debug!(
            "Created new session with will_delay_interval: {:?}",
            session.will_delay_interval
        );
        storage.store_session(session.clone()).await?;
        storage
            .remove_all_inflight_messages(&connect.client_id)
            .await?;
        self.session = Some(session);
        Ok(())
    }

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
                    stored.change_only,
                )
                .await?;
        }

        self.router
            .load_change_only_state(&connect.client_id, session.change_only_state.clone())
            .await;

        session.will_message.clone_from(&connect.will);
        session.will_delay_interval = connect
            .will
            .as_ref()
            .and_then(|w| w.properties.will_delay_interval);

        session.receive_maximum = self.client_receive_maximum;

        session.touch();
        storage.store_session(session.clone()).await?;
        self.session = Some(session);
        Ok(())
    }

    pub(super) async fn deliver_queued_messages(&mut self, client_id: &str) -> Result<()> {
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

            for msg in queued_messages {
                let mut publish = msg.to_publish_packet();
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
}
