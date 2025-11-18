use crate::error::{MqttError, Result};
use crate::packet::subscribe::SubscriptionOptions;
use std::collections::HashMap;

/// A subscription to a topic filter
#[derive(Debug, Clone, PartialEq)]
pub struct Subscription {
    /// Topic filter (may include wildcards)
    pub topic_filter: String,
    /// Subscription options
    pub options: SubscriptionOptions,
}

/// Manages subscriptions and topic matching
#[derive(Debug)]
pub struct SubscriptionManager {
    /// Map of topic filter to subscription
    subscriptions: HashMap<String, Subscription>,
}

impl SubscriptionManager {
    /// Creates a new subscription manager
    #[must_use]
    pub fn new() -> Self {
        Self {
            subscriptions: HashMap::new(),
        }
    }

    /// Adds a subscription
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn add(&mut self, topic_filter: String, subscription: Subscription) -> Result<()> {
        // Validate topic filter
        if !is_valid_topic_filter(&topic_filter) {
            return Err(MqttError::InvalidTopicFilter(topic_filter));
        }

        self.subscriptions.insert(topic_filter, subscription);
        Ok(())
    }

    /// Removes a subscription
    ///
    /// Returns `Ok(true)` if the subscription existed and was removed,
    /// `Ok(false)` if the subscription did not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn remove(&mut self, topic_filter: &str) -> Result<bool> {
        Ok(self.subscriptions.remove(topic_filter).is_some())
    }

    #[must_use]
    /// Gets subscriptions matching a topic name
    pub fn matching_subscriptions(&self, topic: &str) -> Vec<(String, Subscription)> {
        self.subscriptions
            .iter()
            .filter(|(filter, _)| topic_matches(topic, filter))
            .map(|(filter, sub)| (filter.clone(), sub.clone()))
            .collect()
    }

    #[must_use]
    /// Gets a specific subscription
    pub fn get(&self, topic_filter: &str) -> Option<&Subscription> {
        self.subscriptions.get(topic_filter)
    }

    #[must_use]
    /// Gets all subscriptions
    pub fn all(&self) -> HashMap<String, Subscription> {
        self.subscriptions.clone()
    }

    #[must_use]
    /// Gets the number of subscriptions
    pub fn count(&self) -> usize {
        self.subscriptions.len()
    }

    /// Clears all subscriptions
    pub fn clear(&mut self) {
        self.subscriptions.clear();
    }

    #[must_use]
    /// Checks if a topic filter exists
    pub fn contains(&self, topic_filter: &str) -> bool {
        self.subscriptions.contains_key(topic_filter)
    }
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[must_use]
/// Checks if a topic matches a topic filter with wildcards
pub fn topic_matches(topic: &str, filter: &str) -> bool {
    let topic_parts: Vec<&str> = topic.split('/').collect();
    let filter_parts: Vec<&str> = filter.split('/').collect();

    topic_matches_recursive(&topic_parts, &filter_parts, 0, 0)
}

fn topic_matches_recursive(
    topic_parts: &[&str],
    filter_parts: &[&str],
    topic_idx: usize,
    filter_idx: usize,
) -> bool {
    // If we've consumed all filter parts
    if filter_idx >= filter_parts.len() {
        // We should have consumed all topic parts too
        return topic_idx >= topic_parts.len();
    }

    let filter_part = filter_parts[filter_idx];

    // Handle multi-level wildcard
    if filter_part == "#" {
        // # must be the last part of the filter
        return filter_idx == filter_parts.len() - 1;
    }

    // If we've run out of topic parts but still have filter parts (and it's not #)
    if topic_idx >= topic_parts.len() {
        return false;
    }

    let topic_part = topic_parts[topic_idx];

    // Handle single-level wildcard or exact match
    if filter_part == "+" || filter_part == topic_part {
        // Move to next level
        return topic_matches_recursive(topic_parts, filter_parts, topic_idx + 1, filter_idx + 1);
    }

    false
}

#[must_use]
/// Validates a topic filter
pub fn is_valid_topic_filter(filter: &str) -> bool {
    // Empty filter is invalid
    if filter.is_empty() {
        return false;
    }

    // Check for null characters
    if filter.contains('\0') {
        return false;
    }

    let parts: Vec<&str> = filter.split('/').collect();

    for (i, part) in parts.iter().enumerate() {
        // # must be alone in its level and must be the last level
        if part.contains('#') && (*part != "#" || i != parts.len() - 1) {
            return false;
        }

        // + must be alone in its level
        if part.contains('+') && *part != "+" {
            return false;
        }
    }

    true
}

#[must_use]
/// Validates a topic name (no wildcards allowed)
pub fn is_valid_topic_name(topic: &str) -> bool {
    // Empty topic is invalid
    if topic.is_empty() {
        return false;
    }

    // Check for null characters
    if topic.contains('\0') {
        return false;
    }

    // Topic names cannot contain wildcards
    if topic.contains('+') || topic.contains('#') {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
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

        // # alone should match everything
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
    fn test_valid_topic_name() {
        assert!(is_valid_topic_name("sport/tennis/player1"));
        assert!(is_valid_topic_name("sport"));
        assert!(is_valid_topic_name("/sport/tennis"));
        assert!(is_valid_topic_name("sport/tennis/"));

        assert!(!is_valid_topic_name(""));
        assert!(!is_valid_topic_name("sport/tennis/+"));
        assert!(!is_valid_topic_name("sport/#"));
        assert!(!is_valid_topic_name("sport/tennis/\0"));
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

        // Test matching
        let matches = manager.matching_subscriptions("sport/tennis/player1");
        assert_eq!(matches.len(), 2);

        let matches = manager.matching_subscriptions("sport/basketball/team1");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, "sport/#");

        // Test removal
        manager.remove("sport/tennis/+").unwrap();
        assert_eq!(manager.count(), 1);
        assert!(!manager.contains("sport/tennis/+"));
    }

    #[test]
    fn test_subscription_manager_edge_cases() {
        let mut manager = SubscriptionManager::new();

        // Test invalid topic filter
        let sub = Subscription {
            topic_filter: "sport/#/invalid".to_string(),
            options: SubscriptionOptions::default(),
        };

        assert!(manager.add("sport/#/invalid".to_string(), sub).is_err());

        // Test overwriting subscription
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
