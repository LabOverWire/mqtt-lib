use crate::error::Result;
use crate::packet::publish::PublishPacket;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Type alias for publish callback functions
pub type PublishCallback = Arc<dyn Fn(PublishPacket) + Send + Sync>;

/// Type alias for callback ID
pub type CallbackId = u64;

/// Entry storing callback with metadata
#[derive(Clone)]
pub(crate) struct CallbackEntry {
    id: CallbackId,
    callback: PublishCallback,
    topic_filter: String,
}

/// Manages message callbacks for topic subscriptions
pub struct CallbackManager {
    /// Callbacks indexed by topic filter for exact matches
    exact_callbacks: Arc<RwLock<HashMap<String, Vec<CallbackEntry>>>>,
    /// Callbacks for wildcard subscriptions
    wildcard_callbacks: Arc<RwLock<Vec<CallbackEntry>>>,
    /// Registry of all callbacks by ID for restoration
    callback_registry: Arc<RwLock<HashMap<CallbackId, CallbackEntry>>>,
    /// Next callback ID
    next_id: Arc<AtomicU64>,
}

impl CallbackManager {
    /// Creates a new callback manager
    #[must_use]
    pub fn new() -> Self {
        Self {
            exact_callbacks: Arc::new(RwLock::new(HashMap::new())),
            wildcard_callbacks: Arc::new(RwLock::new(Vec::new())),
            callback_registry: Arc::new(RwLock::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Registers a callback for a topic filter and returns a callback ID
    ///
    /// # Errors
    ///
    /// Returns an error if a callback with the same ID is already registered
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn register_with_id(
        &self,
        topic_filter: String,
        callback: PublishCallback,
    ) -> Result<CallbackId> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        // Create callback entry
        let entry = CallbackEntry {
            id,
            callback,
            topic_filter: topic_filter.clone(),
        };

        // Store in registry
        self.callback_registry
            .write()
            .await
            .insert(id, entry.clone());

        // Register using existing logic
        self.register_internal(topic_filter, entry).await?;

        Ok(id)
    }

    /// Registers a callback for a topic filter (legacy method)
    ///
    /// # Errors
    ///
    /// Returns an error if a callback with the same ID is already registered
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn register(&self, topic_filter: String, callback: PublishCallback) -> Result<()> {
        self.register_with_id(topic_filter, callback).await?;
        Ok(())
    }

    /// Internal registration logic
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    async fn register_internal(&self, topic_filter: String, entry: CallbackEntry) -> Result<()> {
        // Check if it's a shared subscription
        let actual_filter = if let Some(stripped) = Self::strip_shared_prefix(&topic_filter) {
            // For shared subscriptions, register with the actual topic (without $share prefix)
            stripped.to_string()
        } else {
            topic_filter
        };

        // Check if it's a wildcard subscription
        if actual_filter.contains('+') || actual_filter.contains('#') {
            let mut wildcards = self.wildcard_callbacks.write().await;
            wildcards.push(entry);
        } else {
            // Exact match - use HashMap for O(1) lookup
            let mut exact = self.exact_callbacks.write().await;
            exact
                .entry(actual_filter)
                .or_insert_with(Vec::new)
                .push(entry);
        }
        Ok(())
    }

    /// Gets a callback by ID from the registry
    async fn get_callback(&self, id: CallbackId) -> Option<CallbackEntry> {
        self.callback_registry.read().await.get(&id).cloned()
    }

    /// Re-registers a callback using its stored ID
    ///
    /// # Errors
    ///
    /// Returns an error if the callback cannot be registered
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn restore_callback(&self, id: CallbackId) -> Result<bool> {
        if let Some(entry) = self.get_callback(id).await {
            // Check if callback is already registered
            let topic_filter = &entry.topic_filter;
            let actual_filter = if let Some(stripped) = Self::strip_shared_prefix(topic_filter) {
                stripped.to_string()
            } else {
                topic_filter.clone()
            };

            // Check if already registered
            let already_registered = if actual_filter.contains('+') || actual_filter.contains('#') {
                let wildcards = self.wildcard_callbacks.read().await;
                wildcards.iter().any(|e| e.id == id)
            } else {
                let exact = self.exact_callbacks.read().await;
                exact
                    .get(&actual_filter)
                    .is_some_and(|entries| entries.iter().any(|e| e.id == id))
            };

            if !already_registered {
                self.register_internal(entry.topic_filter.clone(), entry)
                    .await?;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Unregisters all callbacks for a topic filter
    ///
    /// Returns `Ok(true)` if any callbacks were removed, `Ok(false)` if no callbacks existed.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic filter is invalid
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn unregister(&self, topic_filter: &str) -> Result<bool> {
        // Check if it's a shared subscription
        let actual_filter = if let Some(stripped) = Self::strip_shared_prefix(topic_filter) {
            stripped
        } else {
            topic_filter
        };

        // Remove from registry and track if anything was removed
        let mut registry = self.callback_registry.write().await;
        let registry_count_before = registry.len();
        registry.retain(|_, entry| entry.topic_filter != topic_filter);
        let removed_from_registry = registry.len() < registry_count_before;
        drop(registry);

        let removed_from_callbacks = if actual_filter.contains('+') || actual_filter.contains('#') {
            let mut wildcards = self.wildcard_callbacks.write().await;
            let count_before = wildcards.len();
            wildcards.retain(|entry| entry.topic_filter != topic_filter);
            wildcards.len() < count_before
        } else {
            let mut exact = self.exact_callbacks.write().await;
            exact.remove(actual_filter).is_some()
        };

        Ok(removed_from_registry || removed_from_callbacks)
    }

    /// Dispatches a message to all matching callbacks
    ///
    /// # Errors
    ///
    /// Currently always returns Ok, but may return errors in future versions
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn dispatch(&self, message: &PublishPacket) -> Result<()> {
        let mut callbacks_to_call = Vec::new();

        // Check exact matches first (O(1) lookup)
        {
            let exact = self.exact_callbacks.read().await;
            if let Some(entries) = exact.get(&message.topic_name) {
                for entry in entries {
                    callbacks_to_call.push(entry.callback.clone());
                }
            }
        }

        // Check wildcard matches
        {
            let wildcards = self.wildcard_callbacks.read().await;
            for entry in wildcards.iter() {
                let match_filter =
                    Self::strip_shared_prefix(&entry.topic_filter).unwrap_or(&entry.topic_filter);
                if crate::topic_matching::matches(&message.topic_name, match_filter) {
                    callbacks_to_call.push(entry.callback.clone());
                }
            }
        }

        // Call all matching callbacks
        for callback in callbacks_to_call {
            #[cfg(feature = "opentelemetry")]
            {
                use crate::telemetry::propagation;
                let user_props = propagation::extract_user_properties(&message.properties);
                propagation::with_remote_context(&user_props, || {
                    let span = tracing::info_span!(
                        "message_received",
                        topic = %message.topic_name,
                        qos = ?message.qos,
                        payload_size = message.payload.len(),
                        retain = message.retain,
                    );
                    let _enter = span.enter();
                    callback(message.clone());
                });
            }

            #[cfg(not(feature = "opentelemetry"))]
            {
                callback(message.clone());
            }
        }

        Ok(())
    }

    /// Returns the number of registered callbacks
    pub async fn callback_count(&self) -> usize {
        self.callback_registry.read().await.len()
    }

    /// Clears all callbacks
    pub async fn clear(&self) {
        self.exact_callbacks.write().await.clear();
        self.wildcard_callbacks.write().await.clear();
        self.callback_registry.write().await.clear();
    }

    /// Strips the $share/groupname/ prefix from a shared subscription topic filter
    fn strip_shared_prefix(topic_filter: &str) -> Option<&str> {
        if let Some(after_share) = topic_filter.strip_prefix("$share/") {
            // Find the second '/' after $share/
            if let Some(group_end) = after_share.find('/') {
                // Return the topic after $share/groupname/
                Some(&after_share[group_end + 1..])
            } else {
                None
            }
        } else {
            None
        }
    }
}

impl Default for CallbackManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Properties;
    use crate::QoS;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn test_exact_match_callback() {
        let manager = CallbackManager::new();
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        let callback: PublishCallback = Arc::new(move |_msg| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });

        manager
            .register("test/topic".to_string(), callback)
            .await
            .unwrap();

        let message = PublishPacket {
            topic_name: "test/topic".to_string(),
            packet_id: None,
            payload: vec![1, 2, 3],
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
        };

        manager.dispatch(&message).await.unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        // Different topic shouldn't trigger callback
        let message2 = PublishPacket {
            topic_name: "test/other".to_string(),
            ..message.clone()
        };

        manager.dispatch(&message2).await.unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_wildcard_callback() {
        let manager = CallbackManager::new();
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        let callback: PublishCallback = Arc::new(move |_msg| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });

        manager
            .register("test/+/topic".to_string(), callback)
            .await
            .unwrap();

        // Should match
        let message1 = PublishPacket {
            topic_name: "test/foo/topic".to_string(),
            packet_id: None,
            payload: vec![],
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
        };

        manager.dispatch(&message1).await.unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        // Should also match
        let message2 = PublishPacket {
            topic_name: "test/bar/topic".to_string(),
            ..message1.clone()
        };

        manager.dispatch(&message2).await.unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 2);

        // Should not match
        let message3 = PublishPacket {
            topic_name: "test/topic".to_string(),
            ..message1.clone()
        };

        manager.dispatch(&message3).await.unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn test_multiple_callbacks() {
        let manager = CallbackManager::new();
        let counter1 = Arc::new(AtomicU32::new(0));
        let counter2 = Arc::new(AtomicU32::new(0));

        let counter1_clone = Arc::clone(&counter1);
        let callback1: PublishCallback = Arc::new(move |_msg| {
            counter1_clone.fetch_add(1, Ordering::Relaxed);
        });

        let counter2_clone = Arc::clone(&counter2);
        let callback2: PublishCallback = Arc::new(move |_msg| {
            counter2_clone.fetch_add(2, Ordering::Relaxed);
        });

        // Register both callbacks for same topic
        manager
            .register("test/topic".to_string(), callback1)
            .await
            .unwrap();
        manager
            .register("test/topic".to_string(), callback2)
            .await
            .unwrap();

        let message = PublishPacket {
            topic_name: "test/topic".to_string(),
            packet_id: None,
            payload: vec![],
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
        };

        manager.dispatch(&message).await.unwrap();

        assert_eq!(counter1.load(Ordering::Relaxed), 1);
        assert_eq!(counter2.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn test_unregister() {
        let manager = CallbackManager::new();
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        let callback: PublishCallback = Arc::new(move |_msg| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });

        manager
            .register("test/topic".to_string(), callback)
            .await
            .unwrap();

        let message = PublishPacket {
            topic_name: "test/topic".to_string(),
            packet_id: None,
            payload: vec![],
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
        };

        manager.dispatch(&message).await.unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        // Unregister and dispatch again
        manager.unregister("test/topic").await.unwrap();
        manager.dispatch(&message).await.unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 1); // Should not increment
    }

    #[tokio::test]
    async fn test_callback_count() {
        let manager = CallbackManager::new();

        assert_eq!(manager.callback_count().await, 0);

        let callback: PublishCallback = Arc::new(|_msg| {});

        manager
            .register("test/exact".to_string(), callback.clone())
            .await
            .unwrap();
        assert_eq!(manager.callback_count().await, 1);

        manager
            .register("test/+/wildcard".to_string(), callback.clone())
            .await
            .unwrap();
        assert_eq!(manager.callback_count().await, 2);

        manager
            .register("test/exact".to_string(), callback)
            .await
            .unwrap();
        assert_eq!(manager.callback_count().await, 3); // Multiple callbacks for same topic

        manager.clear().await;
        assert_eq!(manager.callback_count().await, 0);
    }

    #[tokio::test]
    async fn test_shared_subscription_callback() {
        let manager = CallbackManager::new();
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        let callback: PublishCallback = Arc::new(move |_msg| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });

        manager
            .register("$share/workers/tasks/#".to_string(), callback)
            .await
            .unwrap();

        let message = PublishPacket {
            topic_name: "tasks/job1".to_string(),
            packet_id: None,
            payload: vec![1, 2, 3],
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
        };

        manager.dispatch(&message).await.unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        let message2 = PublishPacket {
            topic_name: "tasks/job2".to_string(),
            ..message.clone()
        };

        manager.dispatch(&message2).await.unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 2);

        let message3 = PublishPacket {
            topic_name: "other/topic".to_string(),
            ..message.clone()
        };

        manager.dispatch(&message3).await.unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }
}
