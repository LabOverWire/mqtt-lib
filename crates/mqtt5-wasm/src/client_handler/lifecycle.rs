use mqtt5::broker::auth::EnhancedAuthStatus;
use mqtt5_protocol::error::{MqttError, Result};
use mqtt5_protocol::packet::auth::AuthPacket;
use mqtt5_protocol::packet::disconnect::DisconnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use std::sync::Arc;
use tracing::{debug, warn};
use wasm_bindgen_futures::spawn_local;

use crate::transport::WasmWriter;

use super::WasmClientHandler;

impl WasmClientHandler {
    pub(super) fn handle_pingreq(&self, writer: &mut WasmWriter) -> Result<()> {
        self.write_packet(&Packet::PingResp, writer)
    }

    pub(super) fn handle_disconnect(&mut self, _disconnect: DisconnectPacket) -> Result<()> {
        debug!("Client disconnected normally");
        self.normal_disconnect = true;

        if let Some(ref mut session) = self.session {
            session.will_message = None;
            session.will_delay_interval = None;
        }

        Err(MqttError::ClientClosed)
    }

    #[allow(clippy::too_many_lines)]
    pub(super) async fn handle_auth(
        &mut self,
        auth: AuthPacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let reason_code = auth.reason_code;

        match reason_code {
            ReasonCode::ContinueAuthentication => self.handle_continue_auth(auth, writer).await,
            ReasonCode::ReAuthenticate => self.handle_reauth(auth, writer).await,
            _ => {
                warn!("Unexpected AUTH reason code: {:?}", reason_code);
                Ok(())
            }
        }
    }

    async fn handle_continue_auth(
        &mut self,
        auth: AuthPacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        use super::AuthState;

        if self.auth_state != AuthState::InProgress {
            warn!("AUTH received but not in auth flow");
            let disconnect = DisconnectPacket {
                reason_code: ReasonCode::ProtocolError,
                properties: mqtt5_protocol::protocol::v5::properties::Properties::default(),
            };
            self.write_packet(&Packet::Disconnect(disconnect), writer)?;
            return Err(MqttError::ProtocolError(
                "AUTH received outside of auth flow".to_string(),
            ));
        }

        let Some(auth_method) = self.auth_method.clone() else {
            return Err(MqttError::ProtocolError("No auth method set".to_string()));
        };

        let packet_method = auth
            .properties
            .get_authentication_method()
            .cloned()
            .unwrap_or_default();
        if packet_method != auth_method {
            warn!(
                "AUTH method mismatch: expected {}, got {}",
                auth_method, packet_method
            );
            let disconnect = DisconnectPacket {
                reason_code: ReasonCode::ProtocolError,
                properties: mqtt5_protocol::protocol::v5::properties::Properties::default(),
            };
            self.write_packet(&Packet::Disconnect(disconnect), writer)?;
            return Err(MqttError::ProtocolError("AUTH method mismatch".to_string()));
        }

        let auth_data = auth.properties.get_authentication_data();
        let client_id = self
            .pending_connect
            .as_ref()
            .map(|pc| pc.connect.client_id.clone())
            .unwrap_or_default();

        let result = self
            .auth_provider
            .authenticate_enhanced(&auth_method, auth_data, &client_id)
            .await?;

        self.process_enhanced_auth_result(result, writer).await
    }

    async fn handle_reauth(&mut self, auth: AuthPacket, writer: &mut WasmWriter) -> Result<()> {
        use super::AuthState;

        if self.auth_state != AuthState::Completed {
            warn!("Re-auth requested but initial auth not complete");
            let disconnect = DisconnectPacket {
                reason_code: ReasonCode::ProtocolError,
                properties: mqtt5_protocol::protocol::v5::properties::Properties::default(),
            };
            self.write_packet(&Packet::Disconnect(disconnect), writer)?;
            return Err(MqttError::ProtocolError(
                "Re-auth before initial auth".to_string(),
            ));
        }

        let Some(auth_method) = auth.properties.get_authentication_method().cloned() else {
            let disconnect = DisconnectPacket {
                reason_code: ReasonCode::ProtocolError,
                properties: mqtt5_protocol::protocol::v5::properties::Properties::default(),
            };
            self.write_packet(&Packet::Disconnect(disconnect), writer)?;
            return Err(MqttError::ProtocolError(
                "Re-auth missing method".to_string(),
            ));
        };

        let auth_data = auth.properties.get_authentication_data();
        let client_id = self.client_id.clone().unwrap_or_default();

        let result = self
            .auth_provider
            .reauthenticate(&auth_method, auth_data, &client_id, self.user_id.as_deref())
            .await?;

        match result.status {
            EnhancedAuthStatus::Success => {
                debug!("Re-authentication successful for {}", client_id);
                let mut response = AuthPacket::new(ReasonCode::Success);
                response
                    .properties
                    .set_authentication_method(result.auth_method);
                if let Some(data) = result.auth_data {
                    response.properties.set_authentication_data(data.into());
                }
                self.write_packet(&Packet::Auth(response), writer)?;
                Ok(())
            }
            EnhancedAuthStatus::Continue => {
                let mut response = AuthPacket::new(ReasonCode::ContinueAuthentication);
                response
                    .properties
                    .set_authentication_method(result.auth_method);
                if let Some(data) = result.auth_data {
                    response.properties.set_authentication_data(data.into());
                }
                self.write_packet(&Packet::Auth(response), writer)?;
                Ok(())
            }
            EnhancedAuthStatus::Failed => {
                warn!("Re-authentication failed for {}", client_id);
                let disconnect = DisconnectPacket {
                    reason_code: result.reason_code,
                    properties: mqtt5_protocol::protocol::v5::properties::Properties::default(),
                };
                self.write_packet(&Packet::Disconnect(disconnect), writer)?;
                Err(MqttError::AuthenticationFailed)
            }
        }
    }

    async fn authorize_will(&self, client_id: &str, publish: &PublishPacket) -> bool {
        let authorized = self
            .auth_provider
            .authorize_publish(client_id, self.user_id.as_deref(), &publish.topic_name)
            .await;
        if !authorized {
            warn!(
                "Will for {} denied for topic {}",
                client_id, publish.topic_name
            );
            return false;
        }
        true
    }

    pub(super) async fn publish_will_message(&self, client_id: &str) {
        if let Some(ref session) = self.session {
            if let Some(ref will) = session.will_message {
                debug!("Publishing will message for client {}", client_id);

                let mut publish =
                    PublishPacket::new(will.topic.clone(), will.payload.clone(), will.qos);
                publish.retain = will.retain;

                will.properties
                    .apply_to_publish_properties(&mut publish.properties);
                publish.properties.inject_sender(self.user_id.as_deref());
                publish.properties.inject_client_id(Some(client_id));

                if let Some(delay) = session.will_delay_interval {
                    if delay > 0 {
                        debug!("Spawning task to publish will after {} seconds", delay);
                        let router = Arc::clone(&self.router);
                        let auth_provider = Arc::clone(&self.auth_provider);
                        let user_id = self.user_id.clone();
                        let publish_clone = publish.clone();
                        let client_id_clone = client_id.to_string();
                        spawn_local(async move {
                            gloo_timers::future::sleep(std::time::Duration::from_secs(u64::from(
                                delay,
                            )))
                            .await;

                            let authorized = auth_provider
                                .authorize_publish(
                                    &client_id_clone,
                                    user_id.as_deref(),
                                    &publish_clone.topic_name,
                                )
                                .await;
                            if !authorized {
                                warn!(
                                    "Delayed will for {} denied for topic {}",
                                    client_id_clone, publish_clone.topic_name
                                );
                                return;
                            }

                            debug!("Publishing delayed will message for {}", client_id_clone);
                            router.route_message(&publish_clone, None).await;
                        });
                    } else if self.authorize_will(client_id, &publish).await {
                        self.router.route_message(&publish, None).await;
                    }
                } else if self.authorize_will(client_id, &publish).await {
                    self.router.route_message(&publish, None).await;
                }
            }
        }
    }
}
