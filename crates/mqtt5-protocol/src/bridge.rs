use crate::prelude::*;
use crate::types::QoS;
use crate::validation::topic_matches_filter;
use serde::{Deserialize, Serialize};

fn default_qos() -> QoS {
    QoS::AtMostOnce
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BridgeDirection {
    In,
    Out,
    Both,
}

impl BridgeDirection {
    #[must_use]
    pub fn allows_incoming(&self) -> bool {
        matches!(self, Self::In | Self::Both)
    }

    #[must_use]
    pub fn allows_outgoing(&self) -> bool {
        matches!(self, Self::Out | Self::Both)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicMappingCore {
    pub pattern: String,
    pub direction: BridgeDirection,
    #[serde(default = "default_qos")]
    pub qos: QoS,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_prefix: Option<String>,
}

impl TopicMappingCore {
    #[must_use]
    pub fn new(pattern: impl Into<String>, direction: BridgeDirection) -> Self {
        Self {
            pattern: pattern.into(),
            direction,
            qos: QoS::AtMostOnce,
            local_prefix: None,
            remote_prefix: None,
        }
    }

    #[must_use]
    pub fn with_qos(mut self, qos: QoS) -> Self {
        self.qos = qos;
        self
    }

    #[must_use]
    pub fn with_local_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.local_prefix = Some(prefix.into());
        self
    }

    #[must_use]
    pub fn with_remote_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.remote_prefix = Some(prefix.into());
        self
    }

    #[must_use]
    pub fn apply_local_prefix(&self, topic: &str) -> String {
        match &self.local_prefix {
            Some(prefix) => format!("{prefix}{topic}"),
            None => topic.into(),
        }
    }

    #[must_use]
    pub fn apply_remote_prefix(&self, topic: &str) -> String {
        match &self.remote_prefix {
            Some(prefix) => format!("{prefix}{topic}"),
            None => topic.into(),
        }
    }

    #[must_use]
    pub fn matches(&self, topic: &str, is_outgoing: bool) -> bool {
        let direction_matches = match self.direction {
            BridgeDirection::In => !is_outgoing,
            BridgeDirection::Out => is_outgoing,
            BridgeDirection::Both => true,
        };
        direction_matches && topic_matches_filter(topic, &self.pattern)
    }
}

#[derive(Debug, Clone, Default)]
pub struct BridgeStats {
    pub messages_sent: u64,
    pub messages_received: u64,
    pub connection_attempts: u32,
    pub last_error: Option<String>,
}

impl BridgeStats {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_sent(&mut self) {
        self.messages_sent += 1;
    }

    pub fn record_received(&mut self) {
        self.messages_received += 1;
    }

    pub fn record_connection_attempt(&mut self) {
        self.connection_attempts += 1;
    }

    pub fn record_error(&mut self, error: impl Into<String>) {
        self.last_error = Some(error.into());
    }

    pub fn clear_error(&mut self) {
        self.last_error = None;
    }
}

pub struct ForwardingDecision {
    pub transformed_topic: String,
    pub qos: QoS,
}

#[must_use]
pub fn evaluate_forwarding(
    topic: &str,
    mappings: &[TopicMappingCore],
    is_outgoing: bool,
) -> Option<ForwardingDecision> {
    if topic.starts_with("$SYS/") {
        return None;
    }

    for mapping in mappings {
        if mapping.matches(topic, is_outgoing) {
            let transformed = if is_outgoing {
                mapping.apply_remote_prefix(topic)
            } else {
                mapping.apply_local_prefix(topic)
            };

            return Some(ForwardingDecision {
                transformed_topic: transformed,
                qos: mapping.qos,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bridge_direction_helpers() {
        assert!(BridgeDirection::In.allows_incoming());
        assert!(!BridgeDirection::In.allows_outgoing());

        assert!(!BridgeDirection::Out.allows_incoming());
        assert!(BridgeDirection::Out.allows_outgoing());

        assert!(BridgeDirection::Both.allows_incoming());
        assert!(BridgeDirection::Both.allows_outgoing());
    }

    #[test]
    fn test_topic_mapping_prefixes() {
        let mapping = TopicMappingCore::new("sensors/#", BridgeDirection::Out)
            .with_qos(QoS::AtLeastOnce)
            .with_local_prefix("local/")
            .with_remote_prefix("remote/");

        assert_eq!(mapping.apply_local_prefix("temp"), "local/temp");
        assert_eq!(mapping.apply_remote_prefix("temp"), "remote/temp");
    }

    #[test]
    fn test_topic_mapping_matches() {
        let mapping = TopicMappingCore::new("sensors/#", BridgeDirection::Out);

        assert!(mapping.matches("sensors/temp", true));
        assert!(!mapping.matches("sensors/temp", false));
        assert!(!mapping.matches("actuators/valve", true));
    }

    #[test]
    fn test_forwarding_decision() {
        let mappings = vec![TopicMappingCore::new("sensors/#", BridgeDirection::Out)
            .with_qos(QoS::AtLeastOnce)
            .with_remote_prefix("bridge/")];

        let decision = evaluate_forwarding("sensors/temp", &mappings, true);
        assert!(decision.is_some());
        let d = decision.unwrap();
        assert_eq!(d.transformed_topic, "bridge/sensors/temp");
        assert_eq!(d.qos, QoS::AtLeastOnce);

        assert!(evaluate_forwarding("$SYS/broker/load", &mappings, true).is_none());
        assert!(evaluate_forwarding("sensors/temp", &mappings, false).is_none());
    }

    #[test]
    fn test_bridge_stats() {
        let mut stats = BridgeStats::new();
        stats.record_sent();
        stats.record_sent();
        stats.record_received();
        stats.record_connection_attempt();
        stats.record_error("connection refused");

        assert_eq!(stats.messages_sent, 2);
        assert_eq!(stats.messages_received, 1);
        assert_eq!(stats.connection_attempts, 1);
        assert_eq!(stats.last_error, Some("connection refused".into()));

        stats.clear_error();
        assert!(stats.last_error.is_none());
    }
}
