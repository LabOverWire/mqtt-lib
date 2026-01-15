use crate::error::Result;
use crate::packet::publish::PublishPacket;
use crate::validation::strip_shared_subscription_prefix;
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
        let actual_filter = strip_shared_subscription_prefix(&topic_filter).to_string();

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
            let topic_filter = &entry.topic_filter;
            let actual_filter = strip_shared_subscription_prefix(topic_filter).to_string();

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
        let actual_filter = strip_shared_subscription_prefix(topic_filter);

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
                let match_filter = strip_shared_subscription_prefix(&entry.topic_filter);
                if crate::topic_matching::matches(&message.topic_name, match_filter) {
                    callbacks_to_call.push(entry.callback.clone());
                }
            }
        }

        for callback in callbacks_to_call {
            let message = message.clone();
            #[cfg(feature = "opentelemetry")]
            {
                use crate::telemetry::propagation;
                let user_props = propagation::extract_user_properties(&message.properties);
                tokio::spawn(async move {
                    propagation::with_remote_context(&user_props, || {
                        let span = tracing::info_span!(
                            "message_received",
                            topic = %message.topic_name,
                            qos = ?message.qos,
                            payload_size = message.payload.len(),
                            retain = message.retain,
                        );
                        let _enter = span.enter();
                        callback(message);
                    });
                });
            }

            #[cfg(not(feature = "opentelemetry"))]
            {
                tokio::spawn(async move {
                    callback(message);
                });
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
            payload: vec![1, 2, 3].into(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
            protocol_version: 5,
        };

        manager.dispatch(&message).await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        let message2 = PublishPacket {
            topic_name: "test/other".to_string(),
            ..message.clone()
        };

        manager.dispatch(&message2).await.unwrap();
        tokio::task::yield_now().await;
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

        let message1 = PublishPacket {
            topic_name: "test/foo/topic".to_string(),
            packet_id: None,
            payload: vec![].into(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
            protocol_version: 5,
        };

        manager.dispatch(&message1).await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        let message2 = PublishPacket {
            topic_name: "test/bar/topic".to_string(),
            ..message1.clone()
        };

        manager.dispatch(&message2).await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(counter.load(Ordering::Relaxed), 2);

        let message3 = PublishPacket {
            topic_name: "test/topic".to_string(),
            ..message1.clone()
        };

        manager.dispatch(&message3).await.unwrap();
        tokio::task::yield_now().await;
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
            payload: vec![].into(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
            protocol_version: 5,
        };

        manager.dispatch(&message).await.unwrap();
        tokio::task::yield_now().await;

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
            payload: vec![].into(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
            protocol_version: 5,
        };

        manager.dispatch(&message).await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        manager.unregister("test/topic").await.unwrap();
        manager.dispatch(&message).await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(counter.load(Ordering::Relaxed), 1);
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
            payload: vec![1, 2, 3].into(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
            protocol_version: 5,
        };

        manager.dispatch(&message).await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        let message2 = PublishPacket {
            topic_name: "tasks/job2".to_string(),
            ..message.clone()
        };

        manager.dispatch(&message2).await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(counter.load(Ordering::Relaxed), 2);

        let message3 = PublishPacket {
            topic_name: "other/topic".to_string(),
            ..message.clone()
        };

        manager.dispatch(&message3).await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn test_dispatch_does_not_block_on_slow_callback() {
        let manager = CallbackManager::new();
        let started = Arc::new(AtomicU32::new(0));
        let started_clone = Arc::clone(&started);

        let callback: PublishCallback = Arc::new(move |_msg| {
            started_clone.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(100));
        });

        manager
            .register("test/topic".to_string(), callback)
            .await
            .unwrap();

        let message = PublishPacket {
            topic_name: "test/topic".to_string(),
            packet_id: None,
            payload: vec![].into(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
            protocol_version: 5,
        };

        let start = std::time::Instant::now();
        manager.dispatch(&message).await.unwrap();
        let dispatch_time = start.elapsed();

        assert!(
            dispatch_time < std::time::Duration::from_millis(50),
            "dispatch should return immediately, took {dispatch_time:?}"
        );

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert_eq!(started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_dispatch_handles_multiple_concurrent_callbacks() {
        let manager = CallbackManager::new();
        let counter = Arc::new(AtomicU32::new(0));

        for i in 0..5 {
            let counter_clone = Arc::clone(&counter);
            let callback: PublishCallback = Arc::new(move |_msg| {
                counter_clone.fetch_add(1, Ordering::SeqCst);
            });
            manager
                .register(format!("test/topic{i}"), callback)
                .await
                .unwrap();
        }

        let wildcard_counter = Arc::clone(&counter);
        let wildcard_callback: PublishCallback = Arc::new(move |_msg| {
            wildcard_counter.fetch_add(10, Ordering::SeqCst);
        });
        manager
            .register("test/#".to_string(), wildcard_callback)
            .await
            .unwrap();

        let message = PublishPacket {
            topic_name: "test/topic0".to_string(),
            packet_id: None,
            payload: vec![].into(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::default(),
            protocol_version: 5,
        };

        manager.dispatch(&message).await.unwrap();
        tokio::task::yield_now().await;

        assert_eq!(counter.load(Ordering::SeqCst), 11);
    }
}
