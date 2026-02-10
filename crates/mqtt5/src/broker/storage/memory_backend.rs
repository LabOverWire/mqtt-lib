//! In-memory storage backend for MQTT broker (testing only)
//!
//! Provides volatile storage for development and testing scenarios.

use super::{
    ClientSession, InflightDirection, InflightMessage, QueuedMessage, RetainedMessage,
    StorageBackend,
};
use crate::error::Result;
use crate::validation::topic_matches_filter;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::debug;

/// In-memory storage backend
#[derive(Debug)]
pub struct MemoryBackend {
    retained: Arc<Mutex<HashMap<String, RetainedMessage>>>,
    sessions: Arc<Mutex<HashMap<String, ClientSession>>>,
    queues: Arc<Mutex<HashMap<String, Vec<QueuedMessage>>>>,
    inflight: Arc<Mutex<HashMap<String, Vec<InflightMessage>>>>,
}

impl MemoryBackend {
    /// Create new memory storage backend
    #[must_use]
    pub fn new() -> Self {
        Self {
            retained: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            queues: Arc::new(Mutex::new(HashMap::new())),
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for MemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageBackend for MemoryBackend {
    async fn store_retained_message(&self, topic: &str, message: RetainedMessage) -> Result<()> {
        let mut retained = self.retained.lock();
        retained.insert(topic.to_string(), message);
        debug!("Stored retained message for topic: {}", topic);
        Ok(())
    }

    async fn get_retained_message(&self, topic: &str) -> Result<Option<RetainedMessage>> {
        let message = {
            let retained = self.retained.lock();
            retained.get(topic).cloned()
        };

        if let Some(ref msg) = message {
            if msg.is_expired() {
                self.remove_retained_message(topic).await?;
                return Ok(None);
            }
        }

        Ok(message)
    }

    async fn remove_retained_message(&self, topic: &str) -> Result<()> {
        let mut retained = self.retained.lock();
        retained.remove(topic);
        debug!("Removed retained message for topic: {}", topic);
        Ok(())
    }

    async fn get_retained_messages(
        &self,
        topic_filter: &str,
    ) -> Result<Vec<(String, RetainedMessage)>> {
        let retained = self.retained.lock();
        let mut messages = Vec::new();

        for (topic, message) in retained.iter() {
            if topic_matches_filter(topic, topic_filter) && !message.is_expired() {
                messages.push((topic.clone(), message.clone()));
            }
        }

        Ok(messages)
    }

    async fn store_session(&self, session: ClientSession) -> Result<()> {
        let client_id = session.client_id.clone();
        let mut sessions = self.sessions.lock();
        sessions.insert(client_id.clone(), session);
        debug!("Stored session for client: {}", client_id);
        Ok(())
    }

    async fn get_session(&self, client_id: &str) -> Result<Option<ClientSession>> {
        let session = {
            let sessions = self.sessions.lock();
            sessions.get(client_id).cloned()
        };

        if let Some(ref sess) = session {
            if sess.is_expired() {
                self.remove_session(client_id).await?;
                return Ok(None);
            }
        }

        Ok(session)
    }

    async fn remove_session(&self, client_id: &str) -> Result<()> {
        let mut sessions = self.sessions.lock();
        sessions.remove(client_id);
        debug!("Removed session for client: {}", client_id);
        Ok(())
    }

    async fn queue_message(&self, message: QueuedMessage) -> Result<()> {
        let mut queues = self.queues.lock();
        let queue = queues.entry(message.client_id.clone()).or_default();
        queue.push(message.clone());
        debug!("Queued message for client: {}", message.client_id);
        Ok(())
    }

    async fn get_queued_messages(&self, client_id: &str) -> Result<Vec<QueuedMessage>> {
        let queues = self.queues.lock();
        if let Some(messages) = queues.get(client_id) {
            let mut valid_messages = Vec::new();
            for message in messages {
                if !message.is_expired() {
                    valid_messages.push(message.clone());
                }
            }
            Ok(valid_messages)
        } else {
            Ok(Vec::new())
        }
    }

    async fn remove_queued_messages(&self, client_id: &str) -> Result<()> {
        let mut queues = self.queues.lock();
        queues.remove(client_id);
        debug!("Removed all queued messages for client: {}", client_id);
        Ok(())
    }

    async fn store_inflight_message(&self, message: InflightMessage) -> Result<()> {
        let mut inflight = self.inflight.lock();
        let entries = inflight.entry(message.client_id.clone()).or_default();
        if let Some(pos) = entries
            .iter()
            .position(|e| e.packet_id == message.packet_id && e.direction == message.direction)
        {
            entries[pos] = message;
        } else {
            entries.push(message);
        }
        Ok(())
    }

    async fn get_inflight_messages(&self, client_id: &str) -> Result<Vec<InflightMessage>> {
        let inflight = self.inflight.lock();
        Ok(inflight
            .get(client_id)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|e| !e.is_expired())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn remove_inflight_message(
        &self,
        client_id: &str,
        packet_id: u16,
        direction: InflightDirection,
    ) -> Result<()> {
        let mut inflight = self.inflight.lock();
        if let Some(entries) = inflight.get_mut(client_id) {
            entries.retain(|e| !(e.packet_id == packet_id && e.direction == direction));
            if entries.is_empty() {
                inflight.remove(client_id);
            }
        }
        Ok(())
    }

    async fn remove_all_inflight_messages(&self, client_id: &str) -> Result<()> {
        let mut inflight = self.inflight.lock();
        inflight.remove(client_id);
        Ok(())
    }

    async fn cleanup_expired(&self) -> Result<()> {
        let mut removed_count = 0;

        {
            let mut retained = self.retained.lock();
            retained.retain(|_, message| {
                if message.is_expired() {
                    removed_count += 1;
                    false
                } else {
                    true
                }
            });
        }

        {
            let mut sessions = self.sessions.lock();
            sessions.retain(|_, session| {
                if session.is_expired() {
                    removed_count += 1;
                    false
                } else {
                    true
                }
            });
        }

        {
            let mut queues = self.queues.lock();
            for queue in queues.values_mut() {
                let original_len = queue.len();
                queue.retain(|message| !message.is_expired());
                removed_count += original_len - queue.len();
            }
            queues.retain(|_, queue| !queue.is_empty());
        }

        {
            let mut inflight = self.inflight.lock();
            for entries in inflight.values_mut() {
                let original_len = entries.len();
                entries.retain(|entry| !entry.is_expired());
                removed_count += original_len - entries.len();
            }
            inflight.retain(|_, entries| !entries.is_empty());
        }

        if removed_count > 0 {
            debug!("Cleaned up {} expired storage entries", removed_count);
        }

        Ok(())
    }
}
