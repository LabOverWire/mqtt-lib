use crate::error::{MqttError, Result};
use crate::packet::disconnect::DisconnectPacket;
use crate::packet::publish::PublishPacket;
use crate::time::Duration;
use crate::transport::PacketIo;
use std::sync::Arc;
use tracing::{debug, warn};

use super::ClientHandler;

impl ClientHandler {
    pub(super) fn handle_disconnect(&mut self, disconnect: &DisconnectPacket) -> Result<()> {
        self.normal_disconnect = true;
        self.disconnect_reason = Some(disconnect.reason_code);

        if let Some(ref mut session) = self.session {
            session.will_message = None;
            session.will_delay_interval = None;
        }

        Err(MqttError::ClientClosed)
    }

    pub(super) async fn handle_pingreq(&mut self) -> Result<()> {
        self.transport
            .write_packet(crate::packet::Packet::PingResp)
            .await
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

                if let Some(delay) = session.will_delay_interval {
                    debug!("Using will delay from session: {} seconds", delay);
                    if delay > 0 {
                        debug!("Spawning task to publish will after {} seconds", delay);
                        let router = Arc::clone(&self.router);
                        let auth_provider = Arc::clone(&self.auth_provider);
                        let user_id = self.user_id.clone();
                        let publish_clone = publish.clone();
                        let client_id_clone = client_id.to_string();
                        let skip_bridges = self.skip_bridge_forwarding;
                        tokio::spawn(async move {
                            debug!(
                                "Task started: waiting {} seconds before publishing will for {}",
                                delay, client_id_clone
                            );
                            tokio::time::sleep(Duration::from_secs(u64::from(delay))).await;

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

                            debug!(
                                "Task completed: publishing delayed will message for {}",
                                client_id_clone
                            );
                            if skip_bridges {
                                router.route_message_local_only(&publish_clone, None).await;
                            } else {
                                router.route_message(&publish_clone, None).await;
                            }
                        });
                        debug!("Spawned delayed will task for {}", client_id);
                    } else {
                        debug!("Publishing will immediately (delay = 0)");
                        if self.authorize_will(client_id, &publish).await {
                            self.route_publish(&publish, None).await;
                        }
                    }
                } else {
                    debug!("Publishing will immediately (no delay specified)");
                    if self.authorize_will(client_id, &publish).await {
                        self.route_publish(&publish, None).await;
                    }
                }
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

    pub(super) fn next_packet_id(&mut self) -> u16 {
        let id = self.next_packet_id;
        self.next_packet_id = if self.next_packet_id == u16::MAX {
            1
        } else {
            self.next_packet_id + 1
        };
        id
    }

    pub(super) fn advance_packet_id_past_inflight(&mut self) {
        let mut candidate = self.next_packet_id;
        for _ in 0..u16::MAX {
            if !self.outbound_inflight.contains_key(&candidate)
                && !self.inflight_publishes.contains_key(&candidate)
            {
                self.next_packet_id = candidate;
                return;
            }
            candidate = if candidate == u16::MAX {
                1
            } else {
                candidate + 1
            };
        }
    }
}
