use crate::broker::auth::EnhancedAuthStatus;
use crate::error::{MqttError, Result};
use crate::packet::auth::AuthPacket;
use crate::packet::disconnect::DisconnectPacket;
use crate::packet::Packet;
use crate::protocol::v5::reason_codes::ReasonCode;

use super::{AuthState, ClientHandler};

impl ClientHandler {
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
                    self.write_to_client(Packet::Disconnect(disconnect)).await?;
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
                    self.write_to_client(Packet::Disconnect(disconnect)).await?;
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
                self.user_id = result.user_id;

                if let Some(pending) = self.pending_connect.take() {
                    self.connack_auth_data = result.auth_data;
                    self.authenticated_connect = Some(pending);
                } else {
                    let success_auth = Self::auth_success(result.auth_method, result.auth_data)?;
                    self.write_to_client(Packet::Auth(success_auth)).await?;
                }
            }
            EnhancedAuthStatus::Continue => {
                let continue_auth =
                    AuthPacket::continue_authentication(result.auth_method, result.auth_data)?;
                self.write_to_client(Packet::Auth(continue_auth)).await?;
            }
            EnhancedAuthStatus::Failed => {
                let reason_string = if self.request_problem_information {
                    result.reason_string
                } else {
                    None
                };
                if self.pending_connect.take().is_some() {
                    let mut connack = self.new_connack(false, result.reason_code);
                    if let Some(reason) = reason_string.filter(|_| self.protocol_version == 5) {
                        connack.properties.set_reason_string(reason);
                    }
                    self.write_to_client(Packet::ConnAck(connack)).await?;
                } else if self.protocol_version == 5 {
                    let mut disconnect = DisconnectPacket::new(result.reason_code);
                    if let Some(reason) = reason_string {
                        disconnect.properties.set_reason_string(reason);
                    }
                    self.write_to_client(Packet::Disconnect(disconnect)).await?;
                }
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
                self.user_id = result.user_id;
                let success_auth = Self::auth_success(result.auth_method, result.auth_data)?;
                self.write_to_client(Packet::Auth(success_auth)).await?;
            }
            EnhancedAuthStatus::Continue => {
                let continue_auth =
                    AuthPacket::continue_authentication(result.auth_method, result.auth_data)?;
                self.write_to_client(Packet::Auth(continue_auth)).await?;
            }
            EnhancedAuthStatus::Failed => {
                if self.protocol_version == 5 {
                    let disconnect = DisconnectPacket::new(result.reason_code);
                    self.write_to_client(Packet::Disconnect(disconnect)).await?;
                }
                return Err(MqttError::AuthenticationFailed);
            }
        }
        Ok(())
    }

    fn auth_success(auth_method: String, auth_data: Option<Vec<u8>>) -> Result<AuthPacket> {
        let mut success = AuthPacket::success(auth_method)?;
        if let Some(data) = auth_data {
            success.properties.set_authentication_data(data.into());
        }
        Ok(success)
    }
}
