use crate::broker::storage::ClientSession;
use crate::error::{MqttError, Result};
use crate::packet::disconnect::DisconnectPacket;
use crate::packet::publish::PublishPacket;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::time::Duration;
use std::sync::Arc;
use tokio::sync::oneshot;
use tracing::{debug, warn};

use super::ClientHandler;

impl ClientHandler {
    pub(super) async fn handle_disconnect(&mut self, disconnect: &DisconnectPacket) -> Result<()> {
        if let Some(requested) = disconnect.properties.get_session_expiry_interval() {
            if self.connect_session_expiry == Some(0) && requested != 0 {
                warn!(
                    client_id = ?self.client_id,
                    requested,
                    "Session Expiry Interval on DISCONNECT after 0 on CONNECT is a Protocol Error"
                );
                self.disconnect_reason = Some(ReasonCode::ProtocolError);
                let reply = DisconnectPacket::new(ReasonCode::ProtocolError);
                self.write_to_client(crate::packet::Packet::Disconnect(reply))
                    .await?;
                return Err(MqttError::ProtocolError(
                    "Session Expiry Interval on DISCONNECT after 0 on CONNECT".to_string(),
                ));
            }
            let granted =
                ClientSession::granted_expiry(Some(requested), self.maximum_session_expiry());
            if let Some(session) = self.session.as_mut() {
                session.expiry_interval = Some(granted);
            }
            if let Some(client_id) = self.client_id.clone() {
                let slot = self.router.lock_session(&client_id).await;
                let stored = self
                    .update_stored_session(move |session| session.expiry_interval = Some(granted))
                    .await;
                drop(slot);
                if let Err(e) = stored {
                    warn!(client_id = %client_id, "Failed to store the DISCONNECT Session Expiry: {e}");
                }
            }
        }

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
        self.write_to_client(crate::packet::Packet::PingResp).await
    }

    pub(super) async fn publish_will_message(
        &self,
        client_id: &str,
        armed_will: Option<oneshot::Receiver<()>>,
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
            debug!(client_id, "Publishing will immediately");
            if self.authorize_will(client_id, &publish).await {
                self.route_publish(&publish, None).await;
            }
            self.router
                .clear_stored_will(client_id, session.connection_token)
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
        let skip_bridges = self.skip_bridge_forwarding;
        tokio::spawn(async move {
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(u64::from(delay))) => {}
                _ = cancelled => {
                    debug!(client_id, "Delayed will cancelled by a new connection");
                    return;
                }
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
            if skip_bridges {
                router.route_message_local_only(&publish, None).await;
            } else {
                router.route_message(&publish, None).await;
            }
        });
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

    #[test]
    fn all_ids_in_use_falls_back_to_reissuing_start() {
        let start = 42u16;
        let (id, _next) = next_free_packet_id(start, |_| true);
        assert_eq!(
            id, start,
            "when every id is in use the fallback reissues start"
        );
    }
}

#[cfg(test)]
mod disconnect_tests {
    use super::super::ClientHandler;
    use crate::broker::auth::AllowAllAuthProvider;
    use crate::broker::config::BrokerConfig;
    use crate::broker::resource_monitor::{ResourceLimits, ResourceMonitor};
    use crate::broker::router::MessageRouter;
    use crate::broker::storage::{DynamicStorage, MemoryBackend, StorageBackend};
    use crate::broker::sys_topics::BrokerStats;
    use crate::broker::transport::BrokerTransport;
    use crate::packet::connect::ConnectPacket;
    use crate::packet::disconnect::DisconnectPacket;
    use crate::protocol::v5::reason_codes::ReasonCode;
    use std::sync::Arc;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::broadcast;

    #[tokio::test]
    async fn disconnect_session_expiry_is_stored_when_the_disconnect_is_processed() {
        let storage = Arc::new(DynamicStorage::Memory(MemoryBackend::new()));
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
            Arc::clone(&router),
            Arc::new(AllowAllAuthProvider),
            Some(Arc::clone(&storage)),
            Arc::new(BrokerStats::new()),
            Arc::new(ResourceMonitor::new(ResourceLimits::default())),
            shutdown_rx,
        );
        let connect = ConnectPacket::new(
            crate::types::ConnectOptions::new("ending")
                .with_session_expiry_interval(3600)
                .protocol_options,
        );
        handler.protocol_version = 5;
        handler.client_id = Some("ending".to_string());
        handler.handle_session(&connect).await.expect("claim");

        let mut disconnect = DisconnectPacket::new(ReasonCode::Success);
        disconnect.properties.set_session_expiry_interval(0);
        assert!(handler.handle_disconnect(&disconnect).await.is_err());

        let stored = storage
            .get_session("ending")
            .await
            .expect("read")
            .expect("session of the live connection");
        assert_eq!(
            stored.expiry_interval,
            Some(0),
            "a claim racing the release must see that the session ended at the DISCONNECT"
        );
    }
}
