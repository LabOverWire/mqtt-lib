//! Message queuing for offline MQTT clients
//!
//! Handles queuing of messages for clients that are temporarily offline.

use super::{QueuedMessage, Storage, StorageBackend};
use crate::error::Result;
use crate::packet::publish::PublishPacket;
use crate::QoS;
use tracing::debug;

/// Message queue manager for offline clients
pub struct MessageQueue<B: StorageBackend> {
    storage: Storage<B>,
}

impl<B: StorageBackend> MessageQueue<B> {
    /// Create new message queue manager
    pub fn new(storage: Storage<B>) -> Self {
        Self { storage }
    }

    /// Queue message for offline client
    pub async fn enqueue(
        &self,
        client_id: &str,
        packet: PublishPacket,
        qos: QoS,
        packet_id: Option<u16>,
    ) -> Result<()> {
        let message = QueuedMessage::new(packet, client_id.to_string(), qos, packet_id);

        debug!("Queueing message for offline client: {}", client_id);
        self.storage.queue_message(message).await
    }

    /// Get all queued messages for client
    pub async fn dequeue_all(&self, client_id: &str) -> Result<Vec<QueuedMessage>> {
        let messages = self.storage.get_queued_messages(client_id).await?;

        if !messages.is_empty() {
            debug!(
                "Retrieved {} queued messages for client: {}",
                messages.len(),
                client_id
            );

            // Remove messages from storage after retrieval
            self.storage.remove_queued_messages(client_id).await?;
        }

        Ok(messages)
    }

    /// Get count of queued messages for client
    pub async fn count(&self, client_id: &str) -> Result<usize> {
        let messages = self.storage.get_queued_messages(client_id).await?;
        Ok(messages.len())
    }

    /// Clear all queued messages for client
    pub async fn clear(&self, client_id: &str) -> Result<()> {
        debug!("Clearing all queued messages for client: {}", client_id);
        self.storage.remove_queued_messages(client_id).await
    }

    /// Check if client has queued messages
    pub async fn has_messages(&self, client_id: &str) -> Result<bool> {
        let count = self.count(client_id).await?;
        Ok(count > 0)
    }
}
