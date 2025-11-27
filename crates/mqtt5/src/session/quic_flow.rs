use crate::transport::flow::{FlowFlags, FlowId, FlowIdGenerator};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowType {
    Control,
    ClientData,
    ServerData,
}

#[derive(Debug, Clone)]
pub struct FlowState {
    pub id: FlowId,
    pub flow_type: FlowType,
    pub flags: FlowFlags,
    pub expire_interval: Option<Duration>,
    pub created_at: Instant,
    pub last_activity: Instant,
    pub subscriptions: Vec<String>,
    pub topic_aliases: HashMap<u16, String>,
    pub pending_packet_ids: Vec<u16>,
}

impl FlowState {
    pub fn new_control() -> Self {
        Self {
            id: FlowId::control(),
            flow_type: FlowType::Control,
            flags: FlowFlags::default(),
            expire_interval: None,
            created_at: Instant::now(),
            last_activity: Instant::now(),
            subscriptions: Vec::new(),
            topic_aliases: HashMap::new(),
            pending_packet_ids: Vec::new(),
        }
    }

    pub fn new_client_data(
        id: FlowId,
        flags: FlowFlags,
        expire_interval: Option<Duration>,
    ) -> Self {
        Self {
            id,
            flow_type: FlowType::ClientData,
            flags,
            expire_interval,
            created_at: Instant::now(),
            last_activity: Instant::now(),
            subscriptions: Vec::new(),
            topic_aliases: HashMap::new(),
            pending_packet_ids: Vec::new(),
        }
    }

    pub fn new_server_data(
        id: FlowId,
        flags: FlowFlags,
        expire_interval: Option<Duration>,
    ) -> Self {
        Self {
            id,
            flow_type: FlowType::ServerData,
            flags,
            expire_interval,
            created_at: Instant::now(),
            last_activity: Instant::now(),
            subscriptions: Vec::new(),
            topic_aliases: HashMap::new(),
            pending_packet_ids: Vec::new(),
        }
    }

    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    pub fn is_expired(&self) -> bool {
        if let Some(interval) = self.expire_interval {
            self.last_activity.elapsed() > interval
        } else {
            false
        }
    }

    pub fn add_subscription(&mut self, topic_filter: String) {
        if !self.subscriptions.contains(&topic_filter) {
            self.subscriptions.push(topic_filter);
        }
    }

    pub fn remove_subscription(&mut self, topic_filter: &str) {
        self.subscriptions.retain(|s| s != topic_filter);
    }

    pub fn set_topic_alias(&mut self, alias: u16, topic: String) {
        self.topic_aliases.insert(alias, topic);
    }

    pub fn get_topic_alias(&self, alias: u16) -> Option<&String> {
        self.topic_aliases.get(&alias)
    }

    pub fn add_pending_packet_id(&mut self, packet_id: u16) {
        if !self.pending_packet_ids.contains(&packet_id) {
            self.pending_packet_ids.push(packet_id);
        }
    }

    pub fn remove_pending_packet_id(&mut self, packet_id: u16) {
        self.pending_packet_ids.retain(|&id| id != packet_id);
    }
}

#[derive(Debug)]
pub struct FlowRegistry {
    flows: HashMap<FlowId, FlowState>,
    id_generator: FlowIdGenerator,
    max_flows: usize,
}

impl FlowRegistry {
    pub fn new(max_flows: usize) -> Self {
        Self {
            flows: HashMap::new(),
            id_generator: FlowIdGenerator::new(),
            max_flows,
        }
    }

    pub fn new_client_flow(
        &mut self,
        flags: FlowFlags,
        expire_interval: Option<Duration>,
    ) -> Option<FlowId> {
        if self.flows.len() >= self.max_flows {
            warn!(
                current = self.flows.len(),
                max = self.max_flows,
                "Flow registry at capacity"
            );
            return None;
        }

        let id = self.id_generator.next_client();
        let state = FlowState::new_client_data(id, flags, expire_interval);
        self.flows.insert(id, state);
        debug!(flow_id = ?id, flow_type = "ClientData", "Flow registered");
        Some(id)
    }

    pub fn new_server_flow(
        &mut self,
        flags: FlowFlags,
        expire_interval: Option<Duration>,
    ) -> Option<FlowId> {
        if self.flows.len() >= self.max_flows {
            warn!(
                current = self.flows.len(),
                max = self.max_flows,
                "Flow registry at capacity"
            );
            return None;
        }

        let id = self.id_generator.next_server();
        let state = FlowState::new_server_data(id, flags, expire_interval);
        self.flows.insert(id, state);
        debug!(flow_id = ?id, flow_type = "ServerData", "Flow registered");
        Some(id)
    }

    pub fn register_flow(&mut self, state: FlowState) -> bool {
        if self.flows.len() >= self.max_flows {
            return false;
        }
        self.flows.insert(state.id, state);
        true
    }

    pub fn get(&self, id: FlowId) -> Option<&FlowState> {
        self.flows.get(&id)
    }

    pub fn get_mut(&mut self, id: FlowId) -> Option<&mut FlowState> {
        self.flows.get_mut(&id)
    }

    pub fn remove(&mut self, id: FlowId) -> Option<FlowState> {
        self.flows.remove(&id)
    }

    pub fn contains(&self, id: FlowId) -> bool {
        self.flows.contains_key(&id)
    }

    pub fn touch(&mut self, id: FlowId) {
        if let Some(state) = self.flows.get_mut(&id) {
            state.touch();
        }
    }

    pub fn expire_flows(&mut self) -> Vec<FlowId> {
        let expired: Vec<FlowId> = self
            .flows
            .iter()
            .filter(|(_, state)| state.is_expired())
            .map(|(id, _)| *id)
            .collect();

        for id in &expired {
            self.flows.remove(id);
        }

        if !expired.is_empty() {
            debug!(
                expired_count = expired.len(),
                remaining = self.flows.len(),
                "Flows expired"
            );
        }

        expired
    }

    pub fn len(&self) -> usize {
        self.flows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.flows.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&FlowId, &FlowState)> {
        self.flows.iter()
    }

    pub fn client_flows(&self) -> impl Iterator<Item = (&FlowId, &FlowState)> {
        self.flows
            .iter()
            .filter(|(_, state)| state.flow_type == FlowType::ClientData)
    }

    pub fn server_flows(&self) -> impl Iterator<Item = (&FlowId, &FlowState)> {
        self.flows
            .iter()
            .filter(|(_, state)| state.flow_type == FlowType::ServerData)
    }

    pub fn clear(&mut self) {
        self.flows.clear();
    }

    pub fn flows_for_subscription(&self, topic_filter: &str) -> Vec<FlowId> {
        self.flows
            .iter()
            .filter(|(_, state)| state.subscriptions.contains(&topic_filter.to_string()))
            .map(|(id, _)| *id)
            .collect()
    }
}

impl Default for FlowRegistry {
    fn default() -> Self {
        Self::new(256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flow_state_new_control() {
        let state = FlowState::new_control();
        assert_eq!(state.flow_type, FlowType::Control);
        assert_eq!(state.id, FlowId::control());
        assert!(state.subscriptions.is_empty());
    }

    #[test]
    fn test_flow_state_new_client_data() {
        let id = FlowId::client(1);
        let flags = FlowFlags::default();
        let state = FlowState::new_client_data(id, flags, Some(Duration::from_secs(60)));

        assert_eq!(state.flow_type, FlowType::ClientData);
        assert_eq!(state.id, id);
        assert_eq!(state.expire_interval, Some(Duration::from_secs(60)));
    }

    #[test]
    fn test_flow_state_subscriptions() {
        let mut state = FlowState::new_control();

        state.add_subscription("test/topic".to_string());
        assert!(state.subscriptions.contains(&"test/topic".to_string()));

        state.add_subscription("test/topic".to_string());
        assert_eq!(state.subscriptions.len(), 1);

        state.remove_subscription("test/topic");
        assert!(state.subscriptions.is_empty());
    }

    #[test]
    fn test_flow_state_topic_aliases() {
        let mut state = FlowState::new_control();

        state.set_topic_alias(1, "test/topic".to_string());
        assert_eq!(state.get_topic_alias(1), Some(&"test/topic".to_string()));
        assert_eq!(state.get_topic_alias(2), None);
    }

    #[test]
    fn test_flow_state_pending_packet_ids() {
        let mut state = FlowState::new_control();

        state.add_pending_packet_id(1);
        state.add_pending_packet_id(2);
        state.add_pending_packet_id(1);
        assert_eq!(state.pending_packet_ids.len(), 2);

        state.remove_pending_packet_id(1);
        assert_eq!(state.pending_packet_ids.len(), 1);
        assert!(state.pending_packet_ids.contains(&2));
    }

    #[test]
    fn test_flow_registry_new_flows() {
        let mut registry = FlowRegistry::new(10);

        let id1 = registry.new_client_flow(FlowFlags::default(), None);
        assert!(id1.is_some());
        let id1 = id1.unwrap();
        assert!(id1.is_client_initiated());

        let id2 = registry.new_server_flow(FlowFlags::default(), None);
        assert!(id2.is_some());
        let id2 = id2.unwrap();
        assert!(id2.is_server_initiated());

        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn test_flow_registry_max_flows() {
        let mut registry = FlowRegistry::new(2);

        registry.new_client_flow(FlowFlags::default(), None);
        registry.new_client_flow(FlowFlags::default(), None);
        let id3 = registry.new_client_flow(FlowFlags::default(), None);
        assert!(id3.is_none());
    }

    #[test]
    fn test_flow_registry_get_and_remove() {
        let mut registry = FlowRegistry::new(10);

        let id = registry
            .new_client_flow(FlowFlags::default(), None)
            .unwrap();
        assert!(registry.contains(id));
        assert!(registry.get(id).is_some());

        let removed = registry.remove(id);
        assert!(removed.is_some());
        assert!(!registry.contains(id));
    }

    #[test]
    fn test_flow_registry_touch() {
        let mut registry = FlowRegistry::new(10);
        let id = registry
            .new_client_flow(FlowFlags::default(), None)
            .unwrap();

        let initial_time = registry.get(id).unwrap().last_activity;
        std::thread::sleep(Duration::from_millis(10));
        registry.touch(id);
        let new_time = registry.get(id).unwrap().last_activity;

        assert!(new_time > initial_time);
    }

    #[test]
    fn test_flow_registry_flows_for_subscription() {
        let mut registry = FlowRegistry::new(10);

        let id1 = registry
            .new_client_flow(FlowFlags::default(), None)
            .unwrap();
        let id2 = registry
            .new_client_flow(FlowFlags::default(), None)
            .unwrap();

        registry
            .get_mut(id1)
            .unwrap()
            .add_subscription("test/#".to_string());
        registry
            .get_mut(id2)
            .unwrap()
            .add_subscription("other/#".to_string());

        let flows = registry.flows_for_subscription("test/#");
        assert_eq!(flows.len(), 1);
        assert!(flows.contains(&id1));
    }
}
