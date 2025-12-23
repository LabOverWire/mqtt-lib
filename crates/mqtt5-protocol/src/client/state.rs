use crate::packet_id::PacketIdGenerator;
use crate::prelude::{HashMap, String, Vec};
use crate::session::{SubscriptionManager, TopicAliasManager};
use crate::types::QoS;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum ClientState {
    #[default]
    Disconnected,
    Connecting,
    AwaitingAuth {
        challenge: Vec<u8>,
    },
    Connected {
        session_present: bool,
    },
    Disconnecting,
}

#[derive(Debug, Clone)]
pub struct PendingSubscribe {
    pub topic_filters: Vec<String>,
    pub qos_levels: Vec<QoS>,
}

#[derive(Debug, Clone)]
pub struct PendingUnsubscribe {
    pub topic_filters: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PendingPublish {
    pub topic: String,
    pub qos: QoS,
    pub retain: bool,
}

#[derive(Debug, Clone)]
pub struct PendingPubRel {
    pub packet_id: u16,
}

#[derive(Debug)]
pub struct ClientSession {
    state: ClientState,
    client_id: String,
    packet_id_gen: PacketIdGenerator,
    subscriptions: SubscriptionManager,
    outbound_topic_aliases: TopicAliasManager,
    inbound_topic_aliases: TopicAliasManager,
    pending_subacks: HashMap<u16, PendingSubscribe>,
    pending_unsubacks: HashMap<u16, PendingUnsubscribe>,
    pending_pubacks: HashMap<u16, PendingPublish>,
    pending_pubrecs: HashMap<u16, PendingPublish>,
    pending_pubrels: HashMap<u16, PendingPubRel>,
    pending_pubcomps: HashMap<u16, PendingPubRel>,
    receive_maximum: u16,
    max_packet_size: u32,
    server_keep_alive: Option<u16>,
    session_expiry_interval: u32,
}

impl ClientSession {
    #[must_use]
    pub fn new(client_id: &str) -> Self {
        Self {
            state: ClientState::Disconnected,
            client_id: String::from(client_id),
            packet_id_gen: PacketIdGenerator::new(),
            subscriptions: SubscriptionManager::new(),
            outbound_topic_aliases: TopicAliasManager::new(0),
            inbound_topic_aliases: TopicAliasManager::new(0),
            pending_subacks: HashMap::new(),
            pending_unsubacks: HashMap::new(),
            pending_pubacks: HashMap::new(),
            pending_pubrecs: HashMap::new(),
            pending_pubrels: HashMap::new(),
            pending_pubcomps: HashMap::new(),
            receive_maximum: 65535,
            max_packet_size: 268_435_455,
            server_keep_alive: None,
            session_expiry_interval: 0,
        }
    }

    #[must_use]
    pub fn state(&self) -> &ClientState {
        &self.state
    }

    pub fn set_state(&mut self, state: ClientState) {
        self.state = state;
    }

    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    #[must_use]
    pub fn next_packet_id(&self) -> u16 {
        self.packet_id_gen.next()
    }

    #[must_use]
    pub fn subscriptions(&self) -> &SubscriptionManager {
        &self.subscriptions
    }

    pub fn subscriptions_mut(&mut self) -> &mut SubscriptionManager {
        &mut self.subscriptions
    }

    #[must_use]
    pub fn outbound_topic_aliases(&self) -> &TopicAliasManager {
        &self.outbound_topic_aliases
    }

    pub fn outbound_topic_aliases_mut(&mut self) -> &mut TopicAliasManager {
        &mut self.outbound_topic_aliases
    }

    #[must_use]
    pub fn inbound_topic_aliases(&self) -> &TopicAliasManager {
        &self.inbound_topic_aliases
    }

    pub fn inbound_topic_aliases_mut(&mut self) -> &mut TopicAliasManager {
        &mut self.inbound_topic_aliases
    }

    pub fn track_pending_suback(&mut self, packet_id: u16, pending: PendingSubscribe) {
        self.pending_subacks.insert(packet_id, pending);
    }

    pub fn remove_pending_suback(&mut self, packet_id: u16) -> Option<PendingSubscribe> {
        self.pending_subacks.remove(&packet_id)
    }

    pub fn track_pending_unsuback(&mut self, packet_id: u16, pending: PendingUnsubscribe) {
        self.pending_unsubacks.insert(packet_id, pending);
    }

    pub fn remove_pending_unsuback(&mut self, packet_id: u16) -> Option<PendingUnsubscribe> {
        self.pending_unsubacks.remove(&packet_id)
    }

    pub fn track_pending_puback(&mut self, packet_id: u16, pending: PendingPublish) {
        self.pending_pubacks.insert(packet_id, pending);
    }

    pub fn remove_pending_puback(&mut self, packet_id: u16) -> Option<PendingPublish> {
        self.pending_pubacks.remove(&packet_id)
    }

    #[must_use]
    pub fn has_pending_puback(&self, packet_id: u16) -> bool {
        self.pending_pubacks.contains_key(&packet_id)
    }

    pub fn track_pending_pubrec(&mut self, packet_id: u16, pending: PendingPublish) {
        self.pending_pubrecs.insert(packet_id, pending);
    }

    pub fn remove_pending_pubrec(&mut self, packet_id: u16) -> Option<PendingPublish> {
        self.pending_pubrecs.remove(&packet_id)
    }

    #[must_use]
    pub fn has_pending_pubrec(&self, packet_id: u16) -> bool {
        self.pending_pubrecs.contains_key(&packet_id)
    }

    pub fn track_pending_pubrel(&mut self, packet_id: u16, pending: PendingPubRel) {
        self.pending_pubrels.insert(packet_id, pending);
    }

    pub fn remove_pending_pubrel(&mut self, packet_id: u16) -> Option<PendingPubRel> {
        self.pending_pubrels.remove(&packet_id)
    }

    #[must_use]
    pub fn has_pending_pubrel(&self, packet_id: u16) -> bool {
        self.pending_pubrels.contains_key(&packet_id)
    }

    pub fn track_pending_pubcomp(&mut self, packet_id: u16, pending: PendingPubRel) {
        self.pending_pubcomps.insert(packet_id, pending);
    }

    pub fn remove_pending_pubcomp(&mut self, packet_id: u16) -> Option<PendingPubRel> {
        self.pending_pubcomps.remove(&packet_id)
    }

    #[must_use]
    pub fn has_pending_pubcomp(&self, packet_id: u16) -> bool {
        self.pending_pubcomps.contains_key(&packet_id)
    }

    #[must_use]
    pub fn receive_maximum(&self) -> u16 {
        self.receive_maximum
    }

    pub fn set_receive_maximum(&mut self, value: u16) {
        self.receive_maximum = value;
    }

    #[must_use]
    pub fn max_packet_size(&self) -> u32 {
        self.max_packet_size
    }

    pub fn set_max_packet_size(&mut self, value: u32) {
        self.max_packet_size = value;
    }

    #[must_use]
    pub fn server_keep_alive(&self) -> Option<u16> {
        self.server_keep_alive
    }

    pub fn set_server_keep_alive(&mut self, value: Option<u16>) {
        self.server_keep_alive = value;
    }

    #[must_use]
    pub fn session_expiry_interval(&self) -> u32 {
        self.session_expiry_interval
    }

    pub fn set_session_expiry_interval(&mut self, value: u32) {
        self.session_expiry_interval = value;
    }

    pub fn update_topic_alias_maximum(&mut self, outbound_max: u16, inbound_max: u16) {
        self.outbound_topic_aliases = TopicAliasManager::new(outbound_max);
        self.inbound_topic_aliases = TopicAliasManager::new(inbound_max);
    }

    pub fn reset(&mut self) {
        self.state = ClientState::Disconnected;
        self.pending_subacks.clear();
        self.pending_unsubacks.clear();
        self.pending_pubacks.clear();
        self.pending_pubrecs.clear();
        self.pending_pubrels.clear();
        self.pending_pubcomps.clear();
        self.outbound_topic_aliases.clear();
        self.inbound_topic_aliases.clear();
    }

    pub fn reset_for_clean_session(&mut self) {
        self.reset();
        self.subscriptions.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_state_default() {
        let state = ClientState::default();
        assert_eq!(state, ClientState::Disconnected);
    }

    #[test]
    fn test_client_session_new() {
        let session = ClientSession::new("test-client");
        assert_eq!(session.client_id(), "test-client");
        assert_eq!(*session.state(), ClientState::Disconnected);
        assert_eq!(session.receive_maximum(), 65535);
    }

    #[test]
    fn test_pending_acks() {
        let mut session = ClientSession::new("test");

        let pending = PendingPublish {
            topic: String::from("test/topic"),
            qos: QoS::AtLeastOnce,
            retain: false,
        };

        session.track_pending_puback(1, pending);
        assert!(session.has_pending_puback(1));
        assert!(!session.has_pending_puback(2));

        let removed = session.remove_pending_puback(1);
        assert!(removed.is_some());
        assert!(!session.has_pending_puback(1));
    }

    #[test]
    fn test_session_reset() {
        let mut session = ClientSession::new("test");

        session.set_state(ClientState::Connected {
            session_present: true,
        });
        session.track_pending_puback(
            1,
            PendingPublish {
                topic: String::from("t"),
                qos: QoS::AtLeastOnce,
                retain: false,
            },
        );

        session.reset();

        assert_eq!(*session.state(), ClientState::Disconnected);
        assert!(!session.has_pending_puback(1));
    }
}
