use crate::packet::publish::PublishPacket;
use crate::QoS;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Storage for retained messages
#[derive(Debug, Clone)]
pub struct RetainedMessageStore {
    /// Map of topic names to retained messages
    messages: Arc<RwLock<HashMap<String, RetainedMessage>>>,
}

/// A retained message
#[derive(Debug, Clone)]
pub struct RetainedMessage {
    /// The topic name
    pub topic: String,
    /// The message payload (empty means clear retained message)
    pub payload: Vec<u8>,
    /// `QoS` level
    pub qos: QoS,
    /// Message properties
    pub properties: crate::protocol::v5::properties::Properties,
}

impl RetainedMessageStore {
    /// Creates a new retained message store
    #[must_use]
    pub fn new() -> Self {
        Self {
            messages: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Stores or clears a retained message
    pub async fn store(&self, topic: String, message: Option<RetainedMessage>) {
        let mut messages = self.messages.write().await;

        if let Some(msg) = message {
            // Store the retained message
            messages.insert(topic, msg);
        } else {
            // Clear the retained message
            messages.remove(&topic);
        }
    }

    /// Gets all retained messages matching a topic filter
    pub async fn get_matching(&self, topic_filter: &str) -> Vec<RetainedMessage> {
        let messages = self.messages.read().await;
        let mut matching = Vec::new();

        for (topic, message) in messages.iter() {
            if crate::session::topic_matches(topic, topic_filter) {
                matching.push(message.clone());
            }
        }

        matching
    }

    /// Gets a specific retained message by exact topic
    pub async fn get(&self, topic: &str) -> Option<RetainedMessage> {
        let messages = self.messages.read().await;
        messages.get(topic).cloned()
    }

    /// Clears all retained messages
    pub async fn clear_all(&self) {
        let mut messages = self.messages.write().await;
        messages.clear();
    }

    /// Gets the number of retained messages
    pub async fn count(&self) -> usize {
        let messages = self.messages.read().await;
        messages.len()
    }
}

impl From<&PublishPacket> for RetainedMessage {
    fn from(packet: &PublishPacket) -> Self {
        Self {
            topic: packet.topic_name.clone(),
            payload: packet.payload.clone(),
            qos: packet.qos,
            properties: packet.properties.clone(),
        }
    }
}

impl Default for RetainedMessageStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::TestMessageBuilder;
    use crate::Properties;

    #[tokio::test]
    async fn test_store_and_retrieve() {
        let store = RetainedMessageStore::new();

        // Store a retained message
        let msg = RetainedMessage {
            topic: "test/topic".to_string(),
            payload: b"test payload".to_vec(),
            qos: QoS::AtLeastOnce,
            properties: Properties::default(),
        };

        store
            .store("test/topic".to_string(), Some(msg.clone()))
            .await;

        // Retrieve the message
        let retrieved = store.get("test/topic").await;
        assert!(retrieved.is_some());

        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.topic, "test/topic");
        assert_eq!(retrieved.payload, b"test payload");
        assert_eq!(retrieved.qos, QoS::AtLeastOnce);
    }

    #[tokio::test]
    async fn test_clear_retained_message() {
        let store = RetainedMessageStore::new();

        // Store a retained message
        let msg = RetainedMessage {
            topic: "test/topic".to_string(),
            payload: b"test payload".to_vec(),
            qos: QoS::AtMostOnce,
            properties: Properties::default(),
        };

        store.store("test/topic".to_string(), Some(msg)).await;
        assert_eq!(store.count().await, 1);

        // Clear the retained message
        store.store("test/topic".to_string(), None).await;
        assert_eq!(store.count().await, 0);

        // Verify it's gone
        let retrieved = store.get("test/topic").await;
        assert!(retrieved.is_none());
    }

    #[tokio::test]
    async fn test_topic_matching() {
        let store = RetainedMessageStore::new();

        // Store multiple retained messages
        let topics = vec![
            "home/room1/temp",
            "home/room1/humidity",
            "home/room2/temp",
            "office/room1/temp",
        ];

        for topic in topics {
            let msg = RetainedMessage {
                topic: topic.to_string(),
                payload: format!("data for {topic}").into_bytes(),
                qos: QoS::AtMostOnce,
                properties: Properties::default(),
            };
            store.store(topic.to_string(), Some(msg)).await;
        }

        // Test exact match
        let matching = store.get_matching("home/room1/temp").await;
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].topic, "home/room1/temp");

        // Test single-level wildcard
        let matching = store.get_matching("home/+/temp").await;
        assert_eq!(matching.len(), 2);

        // Test multi-level wildcard
        let matching = store.get_matching("home/#").await;
        assert_eq!(matching.len(), 3);

        // Test no match
        let matching = store.get_matching("garage/+/temp").await;
        assert_eq!(matching.len(), 0);
    }

    #[tokio::test]
    async fn test_clear_all() {
        let store = RetainedMessageStore::new();

        // Store multiple messages
        let messages = TestMessageBuilder::new()
            .with_topic_prefix("topic")
            .build_retained_batch(5);

        for (i, msg) in messages.into_iter().enumerate() {
            store.store(format!("topic/{i}"), Some(msg)).await;
        }

        assert_eq!(store.count().await, 5);

        // Clear all
        store.clear_all().await;
        assert_eq!(store.count().await, 0);
    }
}
