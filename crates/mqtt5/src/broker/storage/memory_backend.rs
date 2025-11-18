//! In-memory storage backend for MQTT broker (testing only)
//!
//! Provides volatile storage for development and testing scenarios.

use super::{ClientSession, QueuedMessage, RetainedMessage, StorageBackend};
use crate::error::Result;
use crate::validation::topic_matches_filter;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

/// In-memory storage backend
#[derive(Debug)]
pub struct MemoryBackend {
    /// Retained messages by topic
    retained: Arc<RwLock<HashMap<String, RetainedMessage>>>,
    /// Client sessions by client ID
    sessions: Arc<RwLock<HashMap<String, ClientSession>>>,
    /// Queued messages by client ID
    queues: Arc<RwLock<HashMap<String, Vec<QueuedMessage>>>>,
}

impl MemoryBackend {
    /// Create new memory storage backend
    pub fn new() -> Self {
        Self {
            retained: Arc::new(RwLock::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            queues: Arc::new(RwLock::new(HashMap::new())),
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
        let mut retained = self.retained.write().await;
        retained.insert(topic.to_string(), message);
        debug!("Stored retained message for topic: {}", topic);
        Ok(())
    }

    async fn get_retained_message(&self, topic: &str) -> Result<Option<RetainedMessage>> {
        let retained = self.retained.read().await;
        let message = retained.get(topic).cloned();

        // Check if message has expired
        if let Some(ref msg) = message {
            if msg.is_expired() {
                drop(retained);
                self.remove_retained_message(topic).await?;
                return Ok(None);
            }
        }

        Ok(message)
    }

    async fn remove_retained_message(&self, topic: &str) -> Result<()> {
        let mut retained = self.retained.write().await;
        retained.remove(topic);
        debug!("Removed retained message for topic: {}", topic);
        Ok(())
    }

    async fn get_retained_messages(
        &self,
        topic_filter: &str,
    ) -> Result<Vec<(String, RetainedMessage)>> {
        let retained = self.retained.read().await;
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
        let mut sessions = self.sessions.write().await;
        sessions.insert(client_id.clone(), session);
        debug!("Stored session for client: {}", client_id);
        Ok(())
    }

    async fn get_session(&self, client_id: &str) -> Result<Option<ClientSession>> {
        let sessions = self.sessions.read().await;
        let session = sessions.get(client_id).cloned();

        // Check if session has expired
        if let Some(ref sess) = session {
            if sess.is_expired() {
                drop(sessions);
                self.remove_session(client_id).await?;
                return Ok(None);
            }
        }

        Ok(session)
    }

    async fn remove_session(&self, client_id: &str) -> Result<()> {
        let mut sessions = self.sessions.write().await;
        sessions.remove(client_id);
        debug!("Removed session for client: {}", client_id);
        Ok(())
    }

    async fn queue_message(&self, message: QueuedMessage) -> Result<()> {
        let mut queues = self.queues.write().await;
        let queue = queues.entry(message.client_id.clone()).or_default();
        queue.push(message.clone());
        debug!("Queued message for client: {}", message.client_id);
        Ok(())
    }

    async fn get_queued_messages(&self, client_id: &str) -> Result<Vec<QueuedMessage>> {
        let queues = self.queues.read().await;
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
        let mut queues = self.queues.write().await;
        queues.remove(client_id);
        debug!("Removed all queued messages for client: {}", client_id);
        Ok(())
    }

    async fn cleanup_expired(&self) -> Result<()> {
        let mut removed_count = 0;

        // Clean expired retained messages
        {
            let mut retained = self.retained.write().await;
            retained.retain(|_, message| {
                if message.is_expired() {
                    removed_count += 1;
                    false
                } else {
                    true
                }
            });
        }

        // Clean expired sessions
        {
            let mut sessions = self.sessions.write().await;
            sessions.retain(|_, session| {
                if session.is_expired() {
                    removed_count += 1;
                    false
                } else {
                    true
                }
            });
        }

        // Clean expired queued messages
        {
            let mut queues = self.queues.write().await;
            for queue in queues.values_mut() {
                let original_len = queue.len();
                queue.retain(|message| !message.is_expired());
                removed_count += original_len - queue.len();
            }
            // Remove empty queues
            queues.retain(|_, queue| !queue.is_empty());
        }

        if removed_count > 0 {
            debug!("Cleaned up {} expired storage entries", removed_count);
        }

        Ok(())
    }
}
