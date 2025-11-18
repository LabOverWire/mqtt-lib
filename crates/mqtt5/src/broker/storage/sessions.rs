//! Client session management for MQTT broker
//!
//! Handles persistent client sessions and subscription storage.

use super::{ClientSession, Storage, StorageBackend};
use crate::error::Result;
use crate::QoS;
use std::collections::HashMap;
use tracing::debug;

/// Session manager for client persistence
pub struct SessionManager<B: StorageBackend> {
    storage: Storage<B>,
}

impl<B: StorageBackend> SessionManager<B> {
    /// Create new session manager
    pub fn new(storage: Storage<B>) -> Self {
        Self { storage }
    }

    /// Create or restore client session
    pub async fn get_or_create_session(
        &self,
        client_id: &str,
        clean_start: bool,
        session_expiry_interval: Option<u32>,
    ) -> Result<ClientSession> {
        if clean_start {
            // Clean start: remove any existing session
            debug!("Clean start for client: {}", client_id);
            let _ = self.storage.remove_session(client_id).await;

            let session = ClientSession::new(
                client_id.to_string(),
                session_expiry_interval.is_some(),
                session_expiry_interval,
            );

            self.storage.store_session(session.clone()).await?;
            Ok(session)
        } else {
            // Try to restore existing session
            if let Some(mut session) = self.storage.get_session(client_id).await {
                debug!("Restored session for client: {}", client_id);
                session.touch(); // Update last seen time
                self.storage.store_session(session.clone()).await?;
                Ok(session)
            } else {
                // Create new persistent session
                debug!("Creating new persistent session for client: {}", client_id);
                let session =
                    ClientSession::new(client_id.to_string(), true, session_expiry_interval);

                self.storage.store_session(session.clone()).await?;
                Ok(session)
            }
        }
    }

    /// Update session subscriptions
    pub async fn update_subscriptions(
        &self,
        client_id: &str,
        subscriptions: HashMap<String, QoS>,
    ) -> Result<()> {
        if let Some(mut session) = self.storage.get_session(client_id).await {
            session.subscriptions = subscriptions;
            session.touch();

            debug!("Updated subscriptions for client: {}", client_id);
            self.storage.store_session(session).await
        } else {
            // Session doesn't exist, this shouldn't happen
            debug!(
                "Attempted to update subscriptions for non-existent session: {}",
                client_id
            );
            Ok(())
        }
    }

    /// Add subscription to session
    pub async fn add_subscription(
        &self,
        client_id: &str,
        topic_filter: &str,
        qos: QoS,
    ) -> Result<()> {
        if let Some(mut session) = self.storage.get_session(client_id).await {
            session.add_subscription(topic_filter.to_string(), qos);
            session.touch();

            debug!(
                "Added subscription {} for client: {}",
                topic_filter, client_id
            );
            self.storage.store_session(session).await
        } else {
            Ok(())
        }
    }

    /// Remove subscription from session
    pub async fn remove_subscription(&self, client_id: &str, topic_filter: &str) -> Result<()> {
        if let Some(mut session) = self.storage.get_session(client_id).await {
            session.remove_subscription(topic_filter);
            session.touch();

            debug!(
                "Removed subscription {} for client: {}",
                topic_filter, client_id
            );
            self.storage.store_session(session).await
        } else {
            Ok(())
        }
    }

    /// Get session subscriptions
    pub async fn get_subscriptions(&self, client_id: &str) -> HashMap<String, QoS> {
        if let Some(session) = self.storage.get_session(client_id).await {
            session.subscriptions
        } else {
            HashMap::new()
        }
    }

    /// Remove session (on disconnect with session expiry = 0)
    pub async fn remove_session(&self, client_id: &str) -> Result<()> {
        debug!("Removing session for client: {}", client_id);
        self.storage.remove_session(client_id).await
    }

    /// Touch session to update last seen time
    pub async fn touch_session(&self, client_id: &str) -> Result<()> {
        if let Some(mut session) = self.storage.get_session(client_id).await {
            session.touch();
            self.storage.store_session(session).await
        } else {
            Ok(())
        }
    }
}
