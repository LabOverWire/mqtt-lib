//! Storage layer for MQTT broker persistence
//!
//! Provides durable storage for retained messages, client sessions, and message queues.
//! Designed for production use with atomic operations and efficient file-based storage.

#[cfg(not(target_arch = "wasm32"))]
pub mod file_backend;
pub mod memory_backend;
pub mod queue;
pub mod retained;
pub mod sessions;

#[cfg(not(target_arch = "wasm32"))]
pub use file_backend::FileBackend;
pub use memory_backend::MemoryBackend;
pub use queue::MessageQueue;
pub use retained::RetainedMessages;
pub use sessions::SessionManager;

#[cfg(test)]
mod tests;

use crate::error::Result;
use crate::packet::publish::PublishPacket;
use crate::time::{Duration, SystemTime};
use crate::QoS;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Retained message with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetainedMessage {
    /// Topic name
    pub topic: String,
    /// Message payload
    pub payload: Vec<u8>,
    /// Quality of Service level
    pub qos: QoS,
    /// Retain flag
    pub retain: bool,
    /// When the message was stored (Unix timestamp in seconds)
    pub stored_at_secs: u64,
    /// Original message expiry interval in seconds (if any)
    pub message_expiry_interval: Option<u32>,
    /// Message expiry time (computed from `stored_at` + interval)
    #[serde(skip)]
    pub expires_at: Option<SystemTime>,
}

/// Stored subscription options for session persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSubscription {
    pub qos: QoS,
    pub no_local: bool,
    pub retain_as_published: bool,
    #[serde(default)]
    pub retain_handling: u8,
    pub subscription_id: Option<u32>,
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u8,
}

fn default_protocol_version() -> u8 {
    5
}

impl StoredSubscription {
    #[must_use]
    pub fn new(qos: QoS) -> Self {
        Self {
            qos,
            no_local: false,
            retain_as_published: false,
            retain_handling: 0,
            subscription_id: None,
            protocol_version: 5,
        }
    }
}

/// Client session information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientSession {
    /// Client identifier
    pub client_id: String,
    /// Whether the session should persist after disconnect
    pub persistent: bool,
    /// Session expiry interval in seconds
    pub expiry_interval: Option<u32>,
    /// Active subscriptions with full options
    pub subscriptions: HashMap<String, StoredSubscription>,
    /// Session creation time
    #[serde(skip, default = "SystemTime::now")]
    pub created_at: SystemTime,
    /// Last activity time
    #[serde(skip, default = "SystemTime::now")]
    pub last_seen: SystemTime,
    /// Will message to publish on abnormal disconnect
    pub will_message: Option<crate::types::WillMessage>,
    /// Will delay interval in seconds (extracted for convenience)
    pub will_delay_interval: Option<u32>,
    /// Client's receive maximum (for broker outbound flow control)
    #[serde(default = "default_receive_maximum")]
    pub receive_maximum: u16,
}

fn default_receive_maximum() -> u16 {
    65535
}

/// Queued message for offline client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedMessage {
    /// Topic name
    pub topic: String,
    /// Message payload
    pub payload: Vec<u8>,
    /// Target client ID
    pub client_id: String,
    /// `QoS` level for delivery
    pub qos: QoS,
    /// When the message was queued (Unix timestamp in seconds)
    pub queued_at_secs: u64,
    /// Original message expiry interval in seconds (if any)
    pub message_expiry_interval: Option<u32>,
    /// Message expiry time (computed from `queued_at` + interval)
    #[serde(skip)]
    pub expires_at: Option<SystemTime>,
    /// Packet ID for `QoS` 1/2 delivery
    pub packet_id: Option<u16>,
}

/// Storage backend trait for broker persistence
pub trait StorageBackend: Send + Sync {
    /// Store a retained message
    fn store_retained_message(
        &self,
        topic: &str,
        message: RetainedMessage,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Retrieve a retained message
    fn get_retained_message(
        &self,
        topic: &str,
    ) -> impl std::future::Future<Output = Result<Option<RetainedMessage>>> + Send;

    /// Remove a retained message
    fn remove_retained_message(
        &self,
        topic: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Get all retained messages matching a topic filter
    fn get_retained_messages(
        &self,
        topic_filter: &str,
    ) -> impl std::future::Future<Output = Result<Vec<(String, RetainedMessage)>>> + Send;

    /// Store client session
    fn store_session(
        &self,
        session: ClientSession,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Retrieve client session
    fn get_session(
        &self,
        client_id: &str,
    ) -> impl std::future::Future<Output = Result<Option<ClientSession>>> + Send;

    /// Remove client session
    fn remove_session(
        &self,
        client_id: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Queue message for offline client
    fn queue_message(
        &self,
        message: QueuedMessage,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Get queued messages for client
    fn get_queued_messages(
        &self,
        client_id: &str,
    ) -> impl std::future::Future<Output = Result<Vec<QueuedMessage>>> + Send;

    /// Remove queued messages for client
    fn remove_queued_messages(
        &self,
        client_id: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Clean up expired messages and sessions
    fn cleanup_expired(&self) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Flush any cached session data to persistent storage (no-op for non-caching backends)
    fn flush_sessions(&self) -> impl std::future::Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }
}

/// In-memory storage for fast access with persistent backing
pub struct Storage<B: StorageBackend> {
    /// Persistent storage backend
    backend: Arc<B>,
    /// In-memory retained messages cache
    retained_cache: Arc<RwLock<HashMap<String, RetainedMessage>>>,
    /// In-memory sessions cache
    sessions_cache: Arc<RwLock<HashMap<String, ClientSession>>>,
    /// Sessions that need to be flushed to backend (write-behind)
    dirty_sessions: Arc<RwLock<HashSet<String>>>,
    /// Flag to signal shutdown
    shutdown: Arc<AtomicBool>,
}

impl<B: StorageBackend + 'static> Storage<B> {
    /// Create new storage with backend
    pub fn new(backend: B) -> Self {
        Self {
            backend: Arc::new(backend),
            retained_cache: Arc::new(RwLock::new(HashMap::new())),
            sessions_cache: Arc::new(RwLock::new(HashMap::new())),
            dirty_sessions: Arc::new(RwLock::new(HashSet::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Initialize storage and load data from backend.
    ///
    /// # Errors
    /// Returns an error if the backend fails to load retained messages.
    pub async fn initialize(&self) -> Result<()> {
        // Load retained messages into cache
        // Use "#" as wildcard to match all topics
        let retained_messages = self.backend.get_retained_messages("#").await?;
        let mut cache = self.retained_cache.write().await;
        for (topic, msg) in retained_messages {
            cache.insert(topic, msg);
        }

        // Load sessions into cache
        // Note: We can't enumerate all sessions without adding a new backend method,
        // so sessions will be loaded on-demand

        Ok(())
    }

    /// Store retained message.
    ///
    /// # Errors
    /// Returns an error if the backend fails to persist the message.
    pub async fn store_retained(&self, topic: &str, message: RetainedMessage) -> Result<()> {
        // Update cache
        self.retained_cache
            .write()
            .await
            .insert(topic.to_string(), message.clone());

        // Persist to backend
        self.backend.store_retained_message(topic, message).await
    }

    /// Get retained message
    pub async fn get_retained(&self, topic: &str) -> Option<RetainedMessage> {
        let cache_result = self.retained_cache.read().await.get(topic).cloned();

        if let Some(msg) = cache_result {
            // Check if cached message is expired
            if msg.is_expired() {
                // Remove from cache and backend
                self.remove_retained(topic).await.ok();
                return None;
            }
            return Some(msg);
        }

        // Not in cache, try loading from backend
        if let Ok(Some(msg)) = self.backend.get_retained_message(topic).await {
            // Backend already checks expiry, so if we get here it's valid
            // Add to cache for future use
            self.retained_cache
                .write()
                .await
                .insert(topic.to_string(), msg.clone());
            Some(msg)
        } else {
            None
        }
    }

    /// Remove retained message.
    ///
    /// # Errors
    /// Returns an error if the backend fails to remove the message.
    pub async fn remove_retained(&self, topic: &str) -> Result<()> {
        // Remove from cache
        self.retained_cache.write().await.remove(topic);

        // Remove from backend
        self.backend.remove_retained_message(topic).await
    }

    /// Get retained messages matching topic filter
    pub async fn get_retained_matching(
        &self,
        topic_filter: &str,
    ) -> Vec<(String, RetainedMessage)> {
        use crate::validation::topic_matches_filter;

        self.retained_cache
            .read()
            .await
            .iter()
            .filter(|(topic, _)| topic_matches_filter(topic, topic_filter))
            .map(|(topic, msg)| (topic.clone(), msg.clone()))
            .collect()
    }

    /// Store client session (write-behind: caches immediately, flushes later).
    pub async fn store_session(&self, session: ClientSession) {
        let client_id = session.client_id.clone();

        self.sessions_cache
            .write()
            .await
            .insert(client_id.clone(), session);

        self.dirty_sessions.write().await.insert(client_id);
    }

    /// Flush all dirty sessions to backend.
    ///
    /// # Errors
    /// Returns an error if the backend fails to persist any session.
    pub async fn flush_sessions(&self) -> Result<()> {
        let to_flush: Vec<String> = self.dirty_sessions.read().await.iter().cloned().collect();

        if !to_flush.is_empty() {
            let cache = self.sessions_cache.read().await;
            let mut failed = Vec::new();

            for client_id in to_flush {
                if let Some(session) = cache.get(&client_id) {
                    if let Err(e) = self.backend.store_session(session.clone()).await {
                        tracing::warn!("failed to persist session {}: {}", client_id, e);
                        failed.push(client_id);
                    } else {
                        self.dirty_sessions.write().await.remove(&client_id);
                    }
                } else {
                    self.dirty_sessions.write().await.remove(&client_id);
                }
            }

            if !failed.is_empty() {
                return Err(crate::error::MqttError::Io(format!(
                    "failed to persist {} sessions",
                    failed.len()
                )));
            }
        }

        self.backend.flush_sessions().await
    }

    /// Start background flush task (call once after initialization).
    pub fn start_flush_task(self: &Arc<Self>, flush_interval: Duration) {
        let storage = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(flush_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                if storage.shutdown.load(Ordering::Relaxed) {
                    if let Err(e) = storage.flush_sessions().await {
                        tracing::warn!("failed to flush sessions on shutdown: {e}");
                    }
                    break;
                }

                if let Err(e) = storage.flush_sessions().await {
                    tracing::warn!("failed to flush sessions: {e}");
                }
            }
        });
    }

    /// Signal shutdown and flush remaining data.
    ///
    /// # Errors
    /// Returns an error if flushing sessions fails.
    pub async fn shutdown(&self) -> Result<()> {
        self.shutdown.store(true, Ordering::Relaxed);
        self.flush_sessions().await
    }

    /// Get client session
    pub async fn get_session(&self, client_id: &str) -> Option<ClientSession> {
        // First check cache
        let cache_result = self.sessions_cache.read().await.get(client_id).cloned();

        if let Some(session) = cache_result {
            // Check if cached session is expired
            if session.is_expired() {
                // Remove from cache and backend
                self.remove_session(client_id).await.ok();
                return None;
            }
            return Some(session);
        }

        // Not in cache, try loading from backend
        if let Ok(Some(session)) = self.backend.get_session(client_id).await {
            // Backend already checks expiry, so if we get here it's valid
            // Add to cache for future use
            self.sessions_cache
                .write()
                .await
                .insert(client_id.to_string(), session.clone());
            Some(session)
        } else {
            None
        }
    }

    /// Remove client session.
    ///
    /// # Errors
    /// Returns an error if the backend fails to remove the session.
    pub async fn remove_session(&self, client_id: &str) -> Result<()> {
        self.sessions_cache.write().await.remove(client_id);
        self.dirty_sessions.write().await.remove(client_id);
        self.backend.remove_session(client_id).await
    }

    /// Queue message for offline client.
    ///
    /// # Errors
    /// Returns an error if the backend fails to queue the message.
    pub async fn queue_message(&self, message: QueuedMessage) -> Result<()> {
        self.backend.queue_message(message).await
    }

    /// Get queued messages for client.
    ///
    /// # Errors
    /// Returns an error if the backend fails to retrieve the messages.
    pub async fn get_queued_messages(&self, client_id: &str) -> Result<Vec<QueuedMessage>> {
        self.backend.get_queued_messages(client_id).await
    }

    /// Remove queued messages for client.
    ///
    /// # Errors
    /// Returns an error if the backend fails to remove the messages.
    pub async fn remove_queued_messages(&self, client_id: &str) -> Result<()> {
        self.backend.remove_queued_messages(client_id).await
    }

    /// Periodic cleanup of expired data.
    ///
    /// # Errors
    /// Returns an error if the backend fails during cleanup.
    pub async fn cleanup_expired(&self) -> Result<()> {
        let now = SystemTime::now();

        // Clean expired retained messages from cache
        {
            let mut cache = self.retained_cache.write().await;
            cache.retain(|_, msg| msg.expires_at.is_none_or(|expiry| expiry > now));
        }

        // Clean expired sessions from cache
        {
            let mut cache = self.sessions_cache.write().await;
            cache.retain(|_, session| {
                if let Some(expiry_interval) = session.expiry_interval {
                    let expiry_time =
                        session.last_seen + Duration::from_secs(u64::from(expiry_interval));
                    expiry_time > now
                } else {
                    true
                }
            });
        }

        // Clean expired data from backend
        self.backend.cleanup_expired().await
    }
}

impl RetainedMessage {
    /// Create new retained message from PUBLISH packet
    #[must_use]
    pub fn new(packet: PublishPacket) -> Self {
        let now = SystemTime::now();
        let stored_at_secs = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let message_expiry_interval = packet.properties.get_message_expiry_interval();
        let expires_at =
            message_expiry_interval.map(|interval| now + Duration::from_secs(u64::from(interval)));

        Self {
            topic: packet.topic_name,
            payload: packet.payload.to_vec(),
            qos: packet.qos,
            retain: packet.retain,
            stored_at_secs,
            message_expiry_interval,
            expires_at,
        }
    }

    /// Recompute `expires_at` from stored fields (call after deserialization)
    pub fn recompute_expiry(&mut self) {
        if let Some(interval) = self.message_expiry_interval {
            let stored_at = SystemTime::UNIX_EPOCH + Duration::from_secs(self.stored_at_secs);
            self.expires_at = Some(stored_at + Duration::from_secs(u64::from(interval)));
        }
    }

    /// Convert to `PublishPacket` for delivery
    #[must_use]
    pub fn to_publish_packet(&self) -> PublishPacket {
        let mut packet = PublishPacket::new(&self.topic, self.payload.clone(), self.qos)
            .with_retain(self.retain);

        if let Some(remaining) = self.remaining_expiry_interval() {
            packet.properties.set_message_expiry_interval(remaining);
        }

        packet
    }

    #[must_use]
    pub fn remaining_expiry_interval(&self) -> Option<u32> {
        self.expires_at.and_then(|expiry| {
            expiry
                .duration_since(SystemTime::now())
                .ok()
                .map(|d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX))
        })
    }

    /// Check if message has expired
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .is_some_and(|expiry| SystemTime::now() > expiry)
    }
}

impl ClientSession {
    /// Create new client session
    #[must_use]
    pub fn new(client_id: String, persistent: bool, expiry_interval: Option<u32>) -> Self {
        let now = SystemTime::now();
        Self {
            client_id,
            persistent,
            expiry_interval,
            subscriptions: HashMap::new(),
            created_at: now,
            last_seen: now,
            will_message: None,
            will_delay_interval: None,
            receive_maximum: 65535,
        }
    }

    /// Create new client session with will message
    #[must_use]
    pub fn new_with_will(
        client_id: String,
        persistent: bool,
        expiry_interval: Option<u32>,
        will_message: Option<crate::types::WillMessage>,
    ) -> Self {
        let now = SystemTime::now();
        let will_delay = will_message
            .as_ref()
            .and_then(|w| w.properties.will_delay_interval);
        Self {
            client_id,
            persistent,
            expiry_interval,
            subscriptions: HashMap::new(),
            created_at: now,
            last_seen: now,
            will_message,
            will_delay_interval: will_delay,
            receive_maximum: 65535,
        }
    }

    /// Update last seen time
    pub fn touch(&mut self) {
        self.last_seen = SystemTime::now();
    }

    /// Add subscription to session
    pub fn add_subscription(&mut self, topic_filter: String, subscription: StoredSubscription) {
        self.subscriptions.insert(topic_filter, subscription);
    }

    /// Remove subscription from session
    pub fn remove_subscription(&mut self, topic_filter: &str) {
        self.subscriptions.remove(topic_filter);
    }

    /// Check if session has expired
    #[must_use]
    pub fn is_expired(&self) -> bool {
        if let Some(expiry_interval) = self.expiry_interval {
            let expiry_time = self.last_seen + Duration::from_secs(u64::from(expiry_interval));
            SystemTime::now() > expiry_time
        } else {
            false
        }
    }
}

impl QueuedMessage {
    /// Create new queued message from PUBLISH packet
    #[must_use]
    pub fn new(packet: PublishPacket, client_id: String, qos: QoS, packet_id: Option<u16>) -> Self {
        let now = SystemTime::now();
        let queued_at_secs = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let message_expiry_interval = packet.properties.get_message_expiry_interval();
        let expires_at =
            message_expiry_interval.map(|interval| now + Duration::from_secs(u64::from(interval)));

        Self {
            topic: packet.topic_name,
            payload: packet.payload.to_vec(),
            client_id,
            qos,
            queued_at_secs,
            message_expiry_interval,
            expires_at,
            packet_id,
        }
    }

    /// Recompute `expires_at` from stored fields (call after deserialization)
    pub fn recompute_expiry(&mut self) {
        if let Some(interval) = self.message_expiry_interval {
            let queued_at = SystemTime::UNIX_EPOCH + Duration::from_secs(self.queued_at_secs);
            self.expires_at = Some(queued_at + Duration::from_secs(u64::from(interval)));
        }
    }

    /// Convert to `PublishPacket` for delivery
    #[must_use]
    pub fn to_publish_packet(&self) -> PublishPacket {
        let mut packet = PublishPacket::new(&self.topic, self.payload.clone(), self.qos);
        packet.packet_id = self.packet_id;

        if let Some(remaining) = self.remaining_expiry_interval() {
            packet.properties.set_message_expiry_interval(remaining);
        }

        packet
    }

    #[must_use]
    pub fn remaining_expiry_interval(&self) -> Option<u32> {
        self.expires_at.and_then(|expiry| {
            expiry
                .duration_since(SystemTime::now())
                .ok()
                .map(|d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX))
        })
    }

    /// Check if message has expired
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .is_some_and(|expiry| SystemTime::now() > expiry)
    }
}

/// Dynamic storage backend that can hold different implementations
pub enum DynamicStorage {
    #[cfg(not(target_arch = "wasm32"))]
    File(FileBackend),
    Memory(MemoryBackend),
}

impl StorageBackend for DynamicStorage {
    async fn store_retained_message(&self, topic: &str, message: RetainedMessage) -> Result<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(backend) => backend.store_retained_message(topic, message).await,
            Self::Memory(backend) => backend.store_retained_message(topic, message).await,
        }
    }

    async fn get_retained_message(&self, topic: &str) -> Result<Option<RetainedMessage>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(backend) => backend.get_retained_message(topic).await,
            Self::Memory(backend) => backend.get_retained_message(topic).await,
        }
    }

    async fn remove_retained_message(&self, topic: &str) -> Result<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(backend) => backend.remove_retained_message(topic).await,
            Self::Memory(backend) => backend.remove_retained_message(topic).await,
        }
    }

    async fn get_retained_messages(
        &self,
        topic_filter: &str,
    ) -> Result<Vec<(String, RetainedMessage)>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(backend) => backend.get_retained_messages(topic_filter).await,
            Self::Memory(backend) => backend.get_retained_messages(topic_filter).await,
        }
    }

    async fn store_session(&self, session: ClientSession) -> Result<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(backend) => backend.store_session(session).await,
            Self::Memory(backend) => backend.store_session(session).await,
        }
    }

    async fn get_session(&self, client_id: &str) -> Result<Option<ClientSession>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(backend) => backend.get_session(client_id).await,
            Self::Memory(backend) => backend.get_session(client_id).await,
        }
    }

    async fn remove_session(&self, client_id: &str) -> Result<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(backend) => backend.remove_session(client_id).await,
            Self::Memory(backend) => backend.remove_session(client_id).await,
        }
    }

    async fn queue_message(&self, message: QueuedMessage) -> Result<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(backend) => backend.queue_message(message).await,
            Self::Memory(backend) => backend.queue_message(message).await,
        }
    }

    async fn get_queued_messages(&self, client_id: &str) -> Result<Vec<QueuedMessage>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(backend) => backend.get_queued_messages(client_id).await,
            Self::Memory(backend) => backend.get_queued_messages(client_id).await,
        }
    }

    async fn remove_queued_messages(&self, client_id: &str) -> Result<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(backend) => backend.remove_queued_messages(client_id).await,
            Self::Memory(backend) => backend.remove_queued_messages(client_id).await,
        }
    }

    async fn cleanup_expired(&self) -> Result<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(backend) => backend.cleanup_expired().await,
            Self::Memory(backend) => backend.cleanup_expired().await,
        }
    }
}

impl DynamicStorage {
    /// # Errors
    /// Returns an error if any session fails to persist.
    #[cfg_attr(target_arch = "wasm32", allow(clippy::unused_async))]
    pub async fn flush_sessions(&self) -> Result<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(backend) => backend.flush_sessions().await,
            Self::Memory(_) => Ok(()),
        }
    }

    /// # Errors
    /// Returns an error if flushing sessions fails.
    #[cfg_attr(target_arch = "wasm32", allow(clippy::unused_async))]
    pub async fn shutdown(&self) -> Result<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(backend) => backend.shutdown().await,
            Self::Memory(_) => Ok(()),
        }
    }
}
