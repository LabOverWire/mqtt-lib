//! Retained message management for MQTT broker
//!
//! Handles storage and retrieval of retained messages with proper expiration.

use super::{RetainedMessage, Storage, StorageBackend};
use crate::error::Result;
use crate::packet::publish::PublishPacket;
use tracing::debug;

/// Retained message manager
pub struct RetainedMessages<B: StorageBackend> {
    storage: Storage<B>,
}

impl<B: StorageBackend> RetainedMessages<B> {
    /// Create new retained message manager
    pub fn new(storage: Storage<B>) -> Self {
        Self { storage }
    }

    /// Store retained message for topic
    pub async fn store(&self, topic: &str, packet: PublishPacket) -> Result<()> {
        if packet.payload.is_empty() {
            // Empty payload means remove retained message
            debug!("Removing retained message for topic: {}", topic);
            self.storage.remove_retained(topic).await
        } else {
            // Store new retained message
            let message = RetainedMessage::new(packet);
            debug!("Storing retained message for topic: {}", topic);
            self.storage.store_retained(topic, message).await
        }
    }

    /// Get retained message for topic
    pub async fn get(&self, topic: &str) -> Option<RetainedMessage> {
        self.storage.get_retained(topic).await
    }

    /// Get all retained messages matching topic filter
    pub async fn get_matching(&self, topic_filter: &str) -> Vec<(String, RetainedMessage)> {
        self.storage.get_retained_matching(topic_filter).await
    }

    /// Remove retained message
    pub async fn remove(&self, topic: &str) -> Result<()> {
        self.storage.remove_retained(topic).await
    }
}
