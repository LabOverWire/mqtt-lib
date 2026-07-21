use crate::error::{MqttError, Result};
use crate::packet::disconnect::DisconnectPacket;
use crate::packet::publish::PublishPacket;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::time::Duration;
use crate::transport::PacketIo;
use std::sync::Arc;
use tracing::{debug, warn};

use super::ClientHandler;

impl ClientHandler {
    pub(super) fn handle_disconnect(&mut self, disconnect: &DisconnectPacket) -> Result<()> {
        self.disconnect_reason = Some(disconnect.reason_code);

        if disconnect.reason_code == ReasonCode::DisconnectWithWillMessage {
            return Err(MqttError::ClientClosed);
        }

        self.normal_disconnect = true;
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
                publish.properties.inject_client_id(Some(client_id));

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
        let (id, next) = next_free_packet_id(self.next_packet_id, |id| {
            self.outbound_inflight.contains_key(&id) || self.inflight_publishes.contains_key(&id)
        });
        self.next_packet_id = next;
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

/// Returns the next unused packet id and the counter to store for the next allocation.
///
/// Advances a `1..=u16::MAX` counter (wrapping `MAX -> 1`, never 0), skipping any id the
/// `in_use` predicate reports as still in flight, so a `u16` wraparound cannot reissue an id
/// whose message is still outstanding and clobber it. If every id is in use (unreachable while
/// Receive Maximum bounds concurrent inflight well below 65535) it falls back to the current id.
fn next_free_packet_id(start: u16, in_use: impl Fn(u16) -> bool) -> (u16, u16) {
    let advance = |id: u16| if id == u16::MAX { 1 } else { id + 1 };
    let mut current = start;
    for _ in 0..u16::MAX {
        let id = current;
        current = advance(current);
        if !in_use(id) {
            return (id, current);
        }
    }
    (current, advance(current))
}

#[cfg(test)]
mod tests {
    use super::next_free_packet_id;
    use std::collections::HashSet;

    #[test]
    fn allocates_sequentially_when_nothing_is_in_use() {
        let (id, next) = next_free_packet_id(1, |_| false);
        assert_eq!(id, 1);
        assert_eq!(next, 2);
    }

    #[test]
    fn wraps_from_max_to_one_never_zero() {
        let (id, next) = next_free_packet_id(u16::MAX, |_| false);
        assert_eq!(id, u16::MAX);
        assert_eq!(next, 1);
    }

    #[test]
    fn skips_ids_still_in_use() {
        let in_use: HashSet<u16> = [1, 2, 3].into_iter().collect();
        let (id, next) = next_free_packet_id(1, |id| in_use.contains(&id));
        assert_eq!(id, 4);
        assert_eq!(next, 5);
    }

    #[test]
    fn does_not_clobber_a_stuck_id_on_wraparound() {
        let stuck = 9u16;
        let (id, _) = next_free_packet_id(stuck, |id| id == stuck);
        assert_ne!(id, stuck);
        assert_eq!(id, stuck + 1);
    }
}
