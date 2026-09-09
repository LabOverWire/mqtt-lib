//! In-memory storage backend for MQTT broker (testing only)
//!
//! Provides volatile storage for development and testing scenarios.

use super::{
    ClientSession, InflightDirection, InflightMessage, QueueHandle, QueueLimits, QueueRegistry,
    QueuedMessage, RetainedMessage, StorageBackend,
};
use crate::error::Result;
use crate::validation::topic_matches_filter;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::debug;

type InflightMap = HashMap<String, HashMap<(u16, InflightDirection), InflightMessage>>;

#[derive(Debug)]
pub struct MemoryBackend {
    retained: Arc<Mutex<HashMap<String, RetainedMessage>>>,
    sessions: Arc<Mutex<HashMap<String, ClientSession>>>,
    queues: QueueRegistry,
    inflight: Arc<Mutex<InflightMap>>,
}

impl MemoryBackend {
    /// Create new memory storage backend
    #[must_use]
    pub fn new() -> Self {
        Self::with_queue_limits(QueueLimits::default())
    }

    #[must_use]
    pub fn with_queue_limits(limits: QueueLimits) -> Self {
        Self {
            retained: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            queues: QueueRegistry::new(limits, None),
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
    fn store_retained_message(
        &self,
        topic: &str,
        message: RetainedMessage,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let mut retained = self.retained.lock();
        retained.insert(topic.to_string(), message);
        debug!("Stored retained message for topic: {}", topic);
        std::future::ready(Ok(()))
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

    fn remove_retained_message(
        &self,
        topic: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let mut retained = self.retained.lock();
        retained.remove(topic);
        debug!("Removed retained message for topic: {}", topic);
        std::future::ready(Ok(()))
    }

    fn get_retained_messages(
        &self,
        topic_filter: &str,
    ) -> impl std::future::Future<Output = Result<Vec<(String, RetainedMessage)>>> + Send {
        let retained = self.retained.lock();
        let mut messages = Vec::new();

        for (topic, message) in retained.iter() {
            if topic_matches_filter(topic, topic_filter) && !message.is_expired() {
                messages.push((topic.clone(), message.clone()));
            }
        }

        std::future::ready(Ok(messages))
    }

    fn store_session(
        &self,
        session: ClientSession,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let client_id = session.client_id.clone();
        let mut sessions = self.sessions.lock();
        sessions.insert(client_id.clone(), session);
        debug!("Stored session for client: {}", client_id);
        std::future::ready(Ok(()))
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

    fn remove_session(
        &self,
        client_id: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let mut sessions = self.sessions.lock();
        sessions.remove(client_id);
        debug!("Removed session for client: {}", client_id);
        std::future::ready(Ok(()))
    }

    fn queue_handle(&self, client_id: &str) -> QueueHandle {
        self.queues.handle(client_id)
    }

    fn queue_message(
        &self,
        message: QueuedMessage,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let client_id = message.client_id.clone();
        self.queues.handle(&client_id).push(message);
        debug!("Queued message for client: {}", client_id);
        std::future::ready(Ok(()))
    }

    async fn get_queued_messages(&self, client_id: &str) -> Result<Vec<QueuedMessage>> {
        Ok(self.queues.handle(client_id).peek_all().await)
    }

    fn remove_queued_messages(
        &self,
        client_id: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.queues.handle(client_id).clear(None);
        debug!("Removed all queued messages for client: {}", client_id);
        std::future::ready(Ok(()))
    }

    fn store_inflight_message(
        &self,
        message: InflightMessage,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let mut inflight = self.inflight.lock();
        let entries = inflight.entry(message.client_id.clone()).or_default();
        let key = (message.packet_id, message.direction);
        entries.insert(key, message);
        std::future::ready(Ok(()))
    }

    fn get_inflight_messages(
        &self,
        client_id: &str,
    ) -> impl std::future::Future<Output = Result<Vec<InflightMessage>>> + Send {
        let inflight = self.inflight.lock();
        let messages = inflight
            .get(client_id)
            .map(|entries| {
                entries
                    .values()
                    .filter(|e| !e.is_expired())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        std::future::ready(Ok(messages))
    }

    fn remove_inflight_message(
        &self,
        client_id: &str,
        packet_id: u16,
        direction: InflightDirection,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let mut inflight = self.inflight.lock();
        if let Some(entries) = inflight.get_mut(client_id) {
            entries.remove(&(packet_id, direction));
            if entries.is_empty() {
                inflight.remove(client_id);
            }
        }
        std::future::ready(Ok(()))
    }

    fn remove_all_inflight_messages(
        &self,
        client_id: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let mut inflight = self.inflight.lock();
        inflight.remove(client_id);
        std::future::ready(Ok(()))
    }

    fn cleanup_expired(&self) -> impl std::future::Future<Output = Result<()>> + Send {
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

        for queue in self.queues.handles() {
            removed_count += queue.purge_expired();
        }
        self.queues.evict_idle();

        {
            let mut inflight = self.inflight.lock();
            for entries in inflight.values_mut() {
                let original_len = entries.len();
                entries.retain(|_, entry| !entry.is_expired());
                removed_count += original_len - entries.len();
            }
            inflight.retain(|_, entries| !entries.is_empty());
        }

        if removed_count > 0 {
            debug!("Cleaned up {} expired storage entries", removed_count);
        }

        std::future::ready(Ok(()))
    }
}
