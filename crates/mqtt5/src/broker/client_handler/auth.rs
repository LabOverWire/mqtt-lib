//! Extended authentication handling

use crate::broker::auth::EnhancedAuthStatus;
use crate::error::{MqttError, Result};
use crate::packet::auth::AuthPacket;
use crate::packet::connack::ConnAckPacket;
use crate::packet::disconnect::DisconnectPacket;
use crate::packet::Packet;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::transport::PacketIo;

use super::{AuthState, ClientHandler};

impl ClientHandler {
    #[allow(clippy::too_many_lines)]
    pub(super) async fn handle_auth(&mut self, auth: AuthPacket) -> Result<()> {
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
                self.handle_continue_auth(&auth_method, &auth, &client_id)
                    .await
            }
            ReasonCode::ReAuthenticate => {
                self.handle_reauthenticate(&auth_method, &auth, &client_id)
                    .await
            }
            _ => {
                if self.protocol_version == 5 {
                    let disconnect = DisconnectPacket::new(ReasonCode::ProtocolError);
                    self.transport
                        .write_packet(Packet::Disconnect(disconnect))
                        .await?;
                }
                Err(MqttError::ProtocolError(format!(
                    "Unexpected AUTH reason code: {:?}",
                    auth.reason_code
                )))
            }
        }
    }

    async fn handle_continue_auth(
        &mut self,
        auth_method: &str,
        auth: &AuthPacket,
        client_id: &str,
    ) -> Result<()> {
        let result = self
            .auth_provider
            .authenticate_enhanced(auth_method, auth.authentication_data(), client_id)
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
                let continue_auth =
                    AuthPacket::continue_authentication(result.auth_method, result.auth_data)?;
                self.transport
                    .write_packet(Packet::Auth(continue_auth))
                    .await?;
            }
            EnhancedAuthStatus::Failed => {
                let failure_auth = AuthPacket::failure(result.reason_code, result.reason_string)?;
                self.transport
                    .write_packet(Packet::Auth(failure_auth))
                    .await?;
                return Err(MqttError::AuthenticationFailed);
            }
        }
        Ok(())
    }

    async fn handle_reauthenticate(
        &mut self,
        auth_method: &str,
        auth: &AuthPacket,
        client_id: &str,
    ) -> Result<()> {
        if self.auth_state != AuthState::Completed {
            return Err(MqttError::ProtocolError(
                "Cannot re-authenticate before initial auth completes".to_string(),
            ));
        }

        let result = self
            .auth_provider
            .reauthenticate(
                auth_method,
                auth.authentication_data(),
                client_id,
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
                let continue_auth =
                    AuthPacket::continue_authentication(result.auth_method, result.auth_data)?;
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
        Ok(())
    }
}
