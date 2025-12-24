use crate::error::{MqttError, Result};
use crate::packet::subscribe::SubscriptionOptions;
use crate::prelude::{HashMap, String, Vec};
use crate::topic_matching::matches as topic_matches;
use crate::validation::is_valid_topic_filter;

#[derive(Debug, Clone, PartialEq)]
pub struct Subscription {
    pub topic_filter: String,
    pub options: SubscriptionOptions,
}

#[derive(Debug)]
pub struct SubscriptionManager {
    subscriptions: HashMap<String, Subscription>,
}

impl SubscriptionManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            subscriptions: HashMap::new(),
        }
    }

    /// # Errors
    /// Returns `InvalidTopicFilter` if the topic filter is invalid.
    pub fn add(&mut self, topic_filter: String, subscription: Subscription) -> Result<()> {
        if !is_valid_topic_filter(&topic_filter) {
            return Err(MqttError::InvalidTopicFilter(topic_filter));
        }

        self.subscriptions.insert(topic_filter, subscription);
        Ok(())
    }

    /// # Errors
    /// This function currently cannot fail but returns Result for API consistency.
    pub fn remove(&mut self, topic_filter: &str) -> Result<bool> {
        Ok(self.subscriptions.remove(topic_filter).is_some())
    }

    #[must_use]
    pub fn matching_subscriptions(&self, topic: &str) -> Vec<(String, Subscription)> {
        self.subscriptions
            .iter()
            .filter(|(filter, _)| topic_matches(topic, filter))
            .map(|(filter, sub)| (filter.clone(), sub.clone()))
            .collect()
    }

    #[must_use]
    pub fn get(&self, topic_filter: &str) -> Option<&Subscription> {
        self.subscriptions.get(topic_filter)
    }

    #[must_use]
    pub fn all(&self) -> HashMap<String, Subscription> {
        self.subscriptions.clone()
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.subscriptions.len()
    }

    pub fn clear(&mut self) {
        self.subscriptions.clear();
    }

    #[must_use]
    pub fn contains(&self, topic_filter: &str) -> bool {
        self.subscriptions.contains_key(topic_filter)
    }
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::ToString;
    use crate::QoS;

    #[test]
    fn test_topic_matching_exact() {
        assert!(topic_matches(
            "sport/tennis/player1",
            "sport/tennis/player1"
        ));
        assert!(!topic_matches(
            "sport/tennis/player1",
            "sport/tennis/player2"
        ));
        assert!(!topic_matches("sport/tennis", "sport/tennis/player1"));
    }

    #[test]
    fn test_topic_matching_single_level_wildcard() {
        assert!(topic_matches("sport/tennis/player1", "sport/tennis/+"));
        assert!(topic_matches("sport/tennis/player2", "sport/tennis/+"));
        assert!(!topic_matches(
            "sport/tennis/player1/ranking",
            "sport/tennis/+"
        ));

        assert!(topic_matches("sport/tennis/player1", "sport/+/player1"));
        assert!(topic_matches("sport/basketball/player1", "sport/+/player1"));

        assert!(topic_matches(
            "sensors/temperature/room1",
            "+/temperature/+"
        ));
        assert!(topic_matches(
            "devices/temperature/kitchen",
            "+/temperature/+"
        ));
    }

    #[test]
    fn test_topic_matching_multi_level_wildcard() {
        assert!(topic_matches("sport/tennis/player1", "sport/#"));
        assert!(topic_matches("sport/tennis/player1/ranking", "sport/#"));
        assert!(topic_matches("sport", "sport/#"));
        assert!(topic_matches(
            "sport/tennis/player1/score/final",
            "sport/tennis/#"
        ));

        assert!(!topic_matches("sports/tennis/player1", "sport/#"));

        assert!(topic_matches("sport/tennis/player1", "#"));
        assert!(topic_matches("anything/at/all", "#"));
        assert!(topic_matches("single", "#"));
    }

    #[test]
    fn test_topic_matching_combined_wildcards() {
        assert!(topic_matches(
            "sport/tennis/player1/score",
            "sport/+/+/score"
        ));
        assert!(topic_matches(
            "sport/tennis/player1/score/final",
            "sport/+/player1/#"
        ));
        assert!(topic_matches("sensors/temperature/room1", "+/+/+"));
        assert!(!topic_matches("sensors/temperature", "+/+/+"));
    }

    #[test]
    fn test_valid_topic_filter() {
        assert!(is_valid_topic_filter("sport/tennis/player1"));
        assert!(is_valid_topic_filter("sport/tennis/+"));
        assert!(is_valid_topic_filter("sport/#"));
        assert!(is_valid_topic_filter("#"));
        assert!(is_valid_topic_filter("+/tennis/+"));
        assert!(is_valid_topic_filter("sport/+/player1/#"));

        assert!(!is_valid_topic_filter(""));
        assert!(!is_valid_topic_filter("sport/tennis#"));
        assert!(!is_valid_topic_filter("sport/#/player"));
        assert!(!is_valid_topic_filter("sport/ten+nis"));
        assert!(!is_valid_topic_filter("sport/tennis/\0"));
    }

    #[test]
    fn test_subscription_manager() {
        let mut manager = SubscriptionManager::new();

        let sub1 = Subscription {
            topic_filter: "sport/tennis/+".to_string(),
            options: SubscriptionOptions::default(),
        };

        let sub2 = Subscription {
            topic_filter: "sport/#".to_string(),
            options: SubscriptionOptions::default().with_qos(QoS::ExactlyOnce),
        };

        manager.add("sport/tennis/+".to_string(), sub1).unwrap();
        manager.add("sport/#".to_string(), sub2).unwrap();

        assert_eq!(manager.count(), 2);
        assert!(manager.contains("sport/tennis/+"));

        let matches = manager.matching_subscriptions("sport/tennis/player1");
        assert_eq!(matches.len(), 2);

        let matches = manager.matching_subscriptions("sport/basketball/team1");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, "sport/#");

        manager.remove("sport/tennis/+").unwrap();
        assert_eq!(manager.count(), 1);
        assert!(!manager.contains("sport/tennis/+"));
    }

    #[test]
    fn test_subscription_manager_edge_cases() {
        let mut manager = SubscriptionManager::new();

        let sub = Subscription {
            topic_filter: "sport/#/invalid".to_string(),
            options: SubscriptionOptions::default(),
        };

        assert!(manager.add("sport/#/invalid".to_string(), sub).is_err());

        let sub1 = Subscription {
            topic_filter: "test/topic".to_string(),
            options: SubscriptionOptions::default().with_qos(QoS::AtMostOnce),
        };

        let sub2 = Subscription {
            topic_filter: "test/topic".to_string(),
            options: SubscriptionOptions::default().with_qos(QoS::AtLeastOnce),
        };

        manager.add("test/topic".to_string(), sub1).unwrap();
        manager.add("test/topic".to_string(), sub2).unwrap();

        assert_eq!(manager.count(), 1);
        assert_eq!(
            manager.get("test/topic").unwrap().options.qos,
            QoS::AtLeastOnce
        );
    }
}
