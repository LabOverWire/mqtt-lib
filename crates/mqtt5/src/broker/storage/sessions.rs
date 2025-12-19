//! Client session management for MQTT broker
//!
//! Handles persistent client sessions and subscription storage.

use super::{ClientSession, Storage, StorageBackend, StoredSubscription};
use crate::error::Result;
use std::collections::HashMap;
use tracing::debug;

/// Session manager for client persistence
pub struct SessionManager<B: StorageBackend> {
    storage: Storage<B>,
}

impl<B: StorageBackend + 'static> SessionManager<B> {
    /// Create new session manager
    #[must_use]
    pub fn new(storage: Storage<B>) -> Self {
        Self { storage }
    }

    /// # Errors
    /// Returns an error if removing an existing session fails.
    pub async fn get_or_create_session(
        &self,
        client_id: &str,
        clean_start: bool,
        session_expiry_interval: Option<u32>,
    ) -> Result<ClientSession> {
        if clean_start {
            debug!("Clean start for client: {}", client_id);
            let _ = self.storage.remove_session(client_id).await;

            let session = ClientSession::new(
                client_id.to_string(),
                session_expiry_interval.is_some(),
                session_expiry_interval,
            );

            self.storage.store_session(session.clone()).await;
            return Ok(session);
        }

        if let Some(mut session) = self.storage.get_session(client_id).await {
            debug!("Restored session for client: {}", client_id);
            session.touch();
            self.storage.store_session(session.clone()).await;
            Ok(session)
        } else {
            debug!("Creating new persistent session for client: {}", client_id);
            let session = ClientSession::new(client_id.to_string(), true, session_expiry_interval);

            self.storage.store_session(session.clone()).await;
            Ok(session)
        }
    }

    pub async fn update_subscriptions(
        &self,
        client_id: &str,
        subscriptions: HashMap<String, StoredSubscription>,
    ) {
        if let Some(mut session) = self.storage.get_session(client_id).await {
            session.subscriptions = subscriptions;
            session.touch();
            debug!("Updated subscriptions for client: {}", client_id);
            self.storage.store_session(session).await;
        } else {
            debug!(
                "Attempted to update subscriptions for non-existent session: {}",
                client_id
            );
        }
    }

    pub async fn add_subscription(
        &self,
        client_id: &str,
        topic_filter: &str,
        subscription: StoredSubscription,
    ) {
        if let Some(mut session) = self.storage.get_session(client_id).await {
            session.add_subscription(topic_filter.to_string(), subscription);
            session.touch();
            debug!(
                "Added subscription {} for client: {}",
                topic_filter, client_id
            );
            self.storage.store_session(session).await;
        }
    }

    pub async fn remove_subscription(&self, client_id: &str, topic_filter: &str) {
        if let Some(mut session) = self.storage.get_session(client_id).await {
            session.remove_subscription(topic_filter);
            session.touch();
            debug!(
                "Removed subscription {} for client: {}",
                topic_filter, client_id
            );
            self.storage.store_session(session).await;
        }
    }

    /// Get session subscriptions
    pub async fn get_subscriptions(&self, client_id: &str) -> HashMap<String, StoredSubscription> {
        if let Some(session) = self.storage.get_session(client_id).await {
            session.subscriptions
        } else {
            HashMap::new()
        }
    }

    /// Remove session (on disconnect with session expiry = 0).
    ///
    /// # Errors
    /// Returns an error if the storage backend fails.
    pub async fn remove_session(&self, client_id: &str) -> Result<()> {
        debug!("Removing session for client: {}", client_id);
        self.storage.remove_session(client_id).await
    }

    pub async fn touch_session(&self, client_id: &str) {
        if let Some(mut session) = self.storage.get_session(client_id).await {
            session.touch();
            self.storage.store_session(session).await;
        }
    }
}
