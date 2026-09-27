use futures::future::{select, Either};
use mqtt5::broker::auth::EnhancedAuthStatus;
use mqtt5::broker::router::Release;
use mqtt5::broker::storage::{unix_millis_now, ClientSession, StorageBackend};
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

    pub(super) async fn handle_disconnect(
        &mut self,
        disconnect: &DisconnectPacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        if let Some(requested) = disconnect.properties.get_session_expiry_interval() {
            if self.connect_session_expiry == Some(0) && requested != 0 {
                warn!(
                    requested,
                    "Session Expiry Interval on DISCONNECT after 0 on CONNECT is a Protocol Error"
                );
                self.write_packet(
                    &Packet::Disconnect(DisconnectPacket::new(ReasonCode::ProtocolError)),
                    writer,
                )?;
                return Err(MqttError::ProtocolError(
                    "Session Expiry Interval on DISCONNECT after 0 on CONNECT".to_string(),
                ));
            }
            let granted =
                ClientSession::granted_expiry(Some(requested), self.maximum_session_expiry());
            if let Some(session) = self.session.as_mut() {
                session.expiry_interval = Some(granted);
                session.persistent = granted != 0;
            }
            if let Some(client_id) = self.client_id.clone() {
                let slot = self.router.lock_session(&client_id).await;
                let stored = self
                    .storage
                    .update_session(&client_id, self.generation, move |session| {
                        session.expiry_interval = Some(granted);
                        session.persistent = granted != 0;
                    })
                    .await;
                drop(slot);
                if let Err(e) = stored {
                    warn!(client_id = %client_id, "Failed to store the DISCONNECT Session Expiry: {e}");
                }
            }
        }

        debug!("Client disconnected normally");

        if disconnect.reason_code != ReasonCode::DisconnectWithWillMessage {
            self.normal_disconnect = true;
            if let Some(ref mut session) = self.session {
                session.will_message = None;
                session.will_delay_interval = None;
            }
        }

        Err(MqttError::ClientClosed)
    }

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

    pub(super) fn session_preserved(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.expiry_interval != Some(0))
    }

    pub(super) async fn release_ownership(
        &self,
        client_id: &str,
    ) -> Option<tokio::sync::oneshot::Receiver<()>> {
        let slot = self.router.lock_session(client_id).await;
        let armed_will = if self.normal_disconnect || self.will_delay() == 0 {
            None
        } else {
            self.router.arm_will(client_id, self.generation).await
        };
        let release = self
            .router
            .release_client(client_id, self.generation, self.session_preserved())
            .await;
        if matches!(release, Release::Owned) {
            self.persist_session_end(client_id).await;
        }
        drop(slot);
        armed_will
    }

    fn will_delay(&self) -> u32 {
        self.session
            .as_ref()
            .and_then(ClientSession::will_publish_delay)
            .unwrap_or(0)
    }

    async fn persist_session_end(&self, client_id: &str) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if session.expiry_interval == Some(0) {
            match self
                .storage
                .remove_owned_session(client_id, self.generation)
                .await
            {
                Ok(true) => {
                    self.storage.queue_handle(client_id).clear(None);
                    if let Err(e) = self.storage.remove_all_inflight_messages(client_id).await {
                        warn!("Failed to remove inflight messages for {client_id}: {e}");
                    }
                }
                Ok(false) => debug!(client_id, "Stored session is not this connection's"),
                Err(e) => warn!("Failed to remove session for {client_id}: {e}"),
            }
            return;
        }
        let expiry_interval = session.expiry_interval;
        let discard_will = self.normal_disconnect;
        let disconnected_at = unix_millis_now();
        let updated = self
            .storage
            .update_session(client_id, self.generation, |stored| {
                stored.mark_disconnected(disconnected_at);
                stored.expiry_interval = expiry_interval;
                if discard_will {
                    stored.will_message = None;
                    stored.will_delay_interval = None;
                }
            })
            .await;
        if let Err(e) = updated {
            warn!("Failed to update session for {client_id}: {e}");
        }
    }

    pub(super) async fn publish_will_message(
        &self,
        client_id: &str,
        armed_will: Option<tokio::sync::oneshot::Receiver<()>>,
    ) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let (Some(will), Some(delay)) = (&session.will_message, session.will_publish_delay())
        else {
            return;
        };

        let mut publish = PublishPacket::new(will.topic.clone(), will.payload.clone(), will.qos);
        publish.retain = will.retain;
        will.properties
            .apply_to_publish_properties(&mut publish.properties);
        publish.properties.inject_sender(self.user_id.as_deref());
        publish.properties.inject_client_id(Some(client_id));

        if delay == 0 {
            if self.authorize_will(client_id, &publish).await {
                self.router.route_message(&publish, None).await;
            }
            self.router
                .clear_stored_will(client_id, self.generation)
                .await;
            return;
        }

        let Some(cancelled) = armed_will else {
            debug!(
                client_id,
                "Delayed will dropped: a new connection for the client id was opened"
            );
            return;
        };

        debug!(client_id, delay, "Scheduling delayed will");
        let router = Arc::clone(&self.router);
        let auth_provider = Arc::clone(&self.auth_provider);
        let user_id = self.user_id.clone();
        let client_id = client_id.to_string();
        let generation = self.generation;
        spawn_local(async move {
            if let Either::Right(_) = select(Box::pin(sleep_secs(delay)), cancelled).await {
                debug!(client_id, "Delayed will cancelled by a new connection");
                return;
            }
            if !router.claim_will(&client_id, generation).await {
                debug!(client_id, "Delayed will cancelled by a new connection");
                return;
            }
            let authorized = auth_provider
                .authorize_publish(&client_id, user_id.as_deref(), &publish.topic_name)
                .await;
            if !authorized {
                warn!(
                    "Delayed will for {client_id} denied for topic {}",
                    publish.topic_name
                );
                return;
            }
            debug!(client_id, "Publishing delayed will");
            router.route_message(&publish, None).await;
        });
    }
}

const MAX_TIMER_MS: u32 = 2_147_483_647;

async fn sleep_secs(secs: u32) {
    let mut remaining_ms = u64::from(secs) * 1000;
    while remaining_ms > 0 {
        let chunk = u32::try_from(remaining_ms).map_or(MAX_TIMER_MS, |ms| ms.min(MAX_TIMER_MS));
        gloo_timers::future::TimeoutFuture::new(chunk).await;
        remaining_ms -= u64::from(chunk);
    }
}
