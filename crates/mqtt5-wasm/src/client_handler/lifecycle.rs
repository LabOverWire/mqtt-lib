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

    pub(super) async fn publish_will_message(&self, client_id: &str) {
        if let Some(ref session) = self.session {
            if let Some(ref will) = session.will_message {
                debug!("Publishing will message for client {}", client_id);

                let mut publish =
                    PublishPacket::new(will.topic.clone(), will.payload.clone(), will.qos);
                publish.retain = will.retain;

                if let Some(format) = will.properties.payload_format_indicator {
                    publish.properties.set_payload_format_indicator(format);
                }
                if let Some(expiry) = will.properties.message_expiry_interval {
                    publish.properties.set_message_expiry_interval(expiry);
                }
                if let Some(ref content_type) = will.properties.content_type {
                    publish.properties.set_content_type(content_type.clone());
                }
                if let Some(ref response_topic) = will.properties.response_topic {
                    publish
                        .properties
                        .set_response_topic(response_topic.clone());
                }
                if let Some(ref correlation_data) = will.properties.correlation_data {
                    publish
                        .properties
                        .set_correlation_data(correlation_data.clone().into());
                }
                for (key, value) in &will.properties.user_properties {
                    publish
                        .properties
                        .add_user_property(key.clone(), value.clone());
                }

                publish
                    .properties
                    .remove_user_property_by_key("x-mqtt-sender");
                if let Some(ref uid) = self.user_id {
                    publish
                        .properties
                        .add_user_property("x-mqtt-sender".into(), uid.clone());
                }

                if let Some(delay) = session.will_delay_interval {
                    if delay > 0 {
                        debug!("Spawning task to publish will after {} seconds", delay);
                        let router = Arc::clone(&self.router);
                        let publish_clone = publish.clone();
                        let client_id_clone = client_id.to_string();
                        spawn_local(async move {
                            gloo_timers::future::sleep(std::time::Duration::from_secs(u64::from(
                                delay,
                            )))
                            .await;
                            debug!("Publishing delayed will message for {}", client_id_clone);
                            router.route_message(&publish_clone, None).await;
                        });
                    } else {
                        self.router.route_message(&publish, None).await;
                    }
                } else {
                    self.router.route_message(&publish, None).await;
                }
            }
        }
    }
}
