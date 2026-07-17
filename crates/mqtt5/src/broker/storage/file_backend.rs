//! File-based storage backend for MQTT broker persistence
//!
//! Provides durable storage using organized file structure with atomic operations.

use super::{
    ClientSession, InflightDirection, InflightMessage, QueuedMessage, RetainedMessage,
    StorageBackend,
};
use crate::error::{MqttError, Result};
use crate::validation::topic_matches_filter;
use serde_json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Disambiguates concurrent writes to the same destination path.
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Storage format version
///
/// IMPORTANT: Only increment this version when the storage format changes:
/// - Modifying `RetainedMessage`, `ClientSession`, or `QueuedMessage` struct fields
/// - Changing file naming scheme (`topic_to_filename`, queue file names)
/// - Changing directory structure (retained/, sessions/, queues/)
///
/// Version History:
/// - 1: Initial version (0.10.0)
const STORAGE_VERSION: &str = "1";

/// File-based storage backend with write-behind session caching
pub struct FileBackend {
    _base_dir: PathBuf,
    retained_dir: PathBuf,
    sessions_dir: PathBuf,
    queues_dir: PathBuf,
    inflight_dir: PathBuf,
    sessions_cache: Arc<RwLock<HashMap<String, ClientSession>>>,
    dirty_sessions: Arc<RwLock<HashSet<String>>>,
    shutdown: Arc<AtomicBool>,
    queue_seq: AtomicU64,
}

impl FileBackend {
    /// Create new file storage backend
    ///
    /// # Errors
    ///
    /// Returns error if directories cannot be created or version mismatch detected
    pub async fn new(base_dir: impl AsRef<Path>) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        let retained_dir = base_dir.join("retained");
        let sessions_dir = base_dir.join("sessions");
        let queues_dir = base_dir.join("queues");
        let inflight_dir = base_dir.join("inflight");

        Self::check_storage_version(&base_dir).await?;

        for dir in [&retained_dir, &sessions_dir, &queues_dir, &inflight_dir] {
            fs::create_dir_all(dir).await.map_err(|e| {
                MqttError::Configuration(format!("Failed to create dir {}: {e}", dir.display()))
            })?;
        }

        info!(
            "Initialized file storage backend at: {}",
            base_dir.display()
        );

        Ok(Self {
            _base_dir: base_dir,
            retained_dir,
            sessions_dir,
            queues_dir,
            inflight_dir,
            sessions_cache: Arc::new(RwLock::new(HashMap::new())),
            dirty_sessions: Arc::new(RwLock::new(HashSet::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
            queue_seq: AtomicU64::new(0),
        })
    }

    /// # Errors
    /// Returns an error if any session fails to persist.
    pub async fn flush_sessions(&self) -> Result<()> {
        let to_flush: Vec<String> = self.dirty_sessions.read().await.iter().cloned().collect();

        if to_flush.is_empty() {
            return Ok(());
        }

        let cache = self.sessions_cache.read().await;
        let mut failed = Vec::new();

        for client_id in to_flush {
            if let Some(session) = cache.get(&client_id) {
                let filename = format!("{client_id}.json");
                let path = self.sessions_dir.join(filename);
                if let Err(e) = self.write_file_atomic(path, session).await {
                    warn!("failed to persist session {}: {}", client_id, e);
                    failed.push(client_id);
                } else {
                    self.dirty_sessions.write().await.remove(&client_id);
                }
            } else {
                self.dirty_sessions.write().await.remove(&client_id);
            }
        }

        if failed.is_empty() {
            Ok(())
        } else {
            Err(MqttError::Io(format!(
                "failed to persist {} sessions",
                failed.len()
            )))
        }
    }

    pub fn start_flush_task(self: &Arc<Self>, flush_interval: std::time::Duration) {
        let backend = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(flush_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                if backend.shutdown.load(Ordering::Relaxed) {
                    if let Err(e) = backend.flush_sessions().await {
                        warn!("failed to flush sessions on shutdown: {e}");
                    }
                    break;
                }

                if let Err(e) = backend.flush_sessions().await {
                    warn!("failed to flush sessions: {e}");
                }
            }
        });
    }

    /// # Errors
    /// Returns an error if flushing sessions fails.
    pub async fn shutdown(&self) -> Result<()> {
        self.shutdown.store(true, Ordering::Relaxed);
        self.flush_sessions().await
    }

    async fn check_storage_version(base_dir: &Path) -> Result<()> {
        let version_file = base_dir.join(".storage_version");

        if version_file.exists() {
            let stored_version = fs::read_to_string(&version_file).await.map_err(|e| {
                MqttError::Configuration(format!("Failed to read storage version: {e}"))
            })?;

            let stored_version = stored_version.trim();

            if stored_version != STORAGE_VERSION {
                return Err(MqttError::Configuration(format!(
                    "Storage version mismatch: found version {}, expected version {}.\n\
                     \n\
                     The storage format has changed and is incompatible.\n\
                     \n\
                     To resolve this issue:\n\
                     1. Backup your data: mqttv5 storage backup --dir {} --output backup.json\n\
                     2. Remove the storage directory: rm -rf {}\n\
                     3. Restart the broker (it will create a new storage with version {})\n\
                     \n\
                     Note: Without backup, all retained messages and session data will be lost.",
                    stored_version,
                    STORAGE_VERSION,
                    base_dir.display(),
                    base_dir.display(),
                    STORAGE_VERSION
                )));
            }

            debug!("Storage version verified: {}", STORAGE_VERSION);
        } else {
            fs::create_dir_all(base_dir).await.map_err(|e| {
                MqttError::Configuration(format!("Failed to create storage dir: {e}"))
            })?;

            fs::write(&version_file, STORAGE_VERSION)
                .await
                .map_err(|e| {
                    MqttError::Configuration(format!("Failed to write storage version: {e}"))
                })?;

            info!("Created new storage with version {}", STORAGE_VERSION);
        }

        Ok(())
    }

    fn topic_to_filename(topic: &str) -> String {
        let mut result = String::with_capacity(topic.len());
        for ch in topic.chars() {
            match ch {
                '%' => result.push_str("%25"),
                '/' => result.push_str("%2F"),
                '+' => result.push_str("%2B"),
                '#' => result.push_str("%23"),
                '$' => result.push_str("%24"),
                '\\' => result.push_str("%5C"),
                ':' => result.push_str("%3A"),
                '*' => result.push_str("%2A"),
                '?' => result.push_str("%3F"),
                '"' => result.push_str("%22"),
                '<' => result.push_str("%3C"),
                '>' => result.push_str("%3E"),
                '|' => result.push_str("%7C"),
                '\0' => result.push_str("%00"),
                _ => result.push(ch),
            }
        }
        result
    }

    fn filename_to_topic(filename: &str) -> String {
        let mut result = String::with_capacity(filename.len());
        let mut chars = filename.chars();
        while let Some(ch) = chars.next() {
            if ch == '%' {
                let hex: String = chars.by_ref().take(2).collect();
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(char::from(byte));
                } else {
                    result.push('%');
                    result.push_str(&hex);
                }
            } else {
                result.push(ch);
            }
        }
        result
    }

    /// Serialises `data` and replaces `path` with it atomically and durably.
    ///
    /// The temp file is named uniquely per write. A name derived only from the destination
    /// would be shared by every concurrent writer of that path, letting one writer's
    /// `create` truncate another's in-flight file; the victim would then sync and rename
    /// zero or partial bytes into place, and a crash before its retry would make that
    /// permanent. The temp file is removed if the write or rename fails.
    async fn write_file_atomic<T: serde::Serialize>(&self, path: PathBuf, data: &T) -> Result<()> {
        let serialized = serde_json::to_vec_pretty(data)
            .map_err(|e| MqttError::Configuration(format!("Failed to serialize data: {e}")))?;

        for attempt in 0..2u8 {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).await.map_err(|e| {
                    MqttError::Io(format!("Failed to create parent directory: {e}"))
                })?;
            }

            let temp_path = path.with_extension(format!(
                "tmp.{}.{}",
                std::process::id(),
                TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));

            match Self::write_temp_file(&temp_path, &serialized).await {
                Ok(()) => {}
                Err(e) => {
                    let _ = fs::remove_file(&temp_path).await;
                    return Err(e);
                }
            }

            match fs::rename(&temp_path, &path).await {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && attempt == 0 => {
                    let _ = fs::remove_file(&temp_path).await;
                    debug!("Atomic write race detected, retrying: {e}");
                }
                Err(e) => {
                    let _ = fs::remove_file(&temp_path).await;
                    return Err(MqttError::Io(format!("Failed to rename temp file: {e}")));
                }
            }
        }

        Ok(())
    }

    /// Writes the payload to `temp_path` and makes it durable before it is renamed into place.
    async fn write_temp_file(temp_path: &Path, serialized: &[u8]) -> Result<()> {
        let mut file = File::create(temp_path)
            .await
            .map_err(|e| MqttError::Io(format!("Failed to create temp file: {e}")))?;

        file.write_all(serialized)
            .await
            .map_err(|e| MqttError::Io(format!("Failed to write temp file: {e}")))?;

        file.flush()
            .await
            .map_err(|e| MqttError::Io(format!("Failed to flush temp file: {e}")))?;

        file.sync_data()
            .await
            .map_err(|e| MqttError::Io(format!("Failed to sync temp file: {e}")))?;

        Ok(())
    }

    /// Reads and deserializes a stored item, reporting an unreadable one as absent.
    ///
    /// Persisted state is untrusted input: it can be truncated by a crash, a full disk, or a
    /// partial write, and it may have been written by an older schema. A single unreadable
    /// item must therefore never fail the surrounding load — that would let data the broker
    /// wrote itself prevent the broker from starting, an outage recoverable only by manually
    /// deleting files. The offending file is quarantined and treated as missing; callers
    /// already skip `None`. Genuine I/O errors still propagate.
    async fn read_file<T: serde::de::DeserializeOwned>(&self, path: PathBuf) -> Result<Option<T>> {
        match fs::read(&path).await {
            Ok(data) => match serde_json::from_slice(&data) {
                Ok(value) => Ok(Some(value)),
                Err(e) => {
                    self.quarantine_unreadable(&path, &e.to_string()).await;
                    Ok(None)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(MqttError::Io(format!(
                "Failed to read {}: {}",
                path.display(),
                e
            ))),
        }
    }

    /// Moves an unreadable file aside, keeping it for diagnosis instead of deleting it.
    ///
    /// The `.corrupt` extension also takes it out of the directory listings, which filter on
    /// the storage extension, so a bad file is reported once rather than on every load.
    async fn quarantine_unreadable(&self, path: &Path, reason: &str) {
        let quarantined = path.with_extension("corrupt");
        if let Err(e) = fs::rename(path, &quarantined).await {
            warn!(
                "Ignoring unreadable storage file {} ({reason}); could not quarantine it: {e}",
                path.display()
            );
            return;
        }
        warn!(
            "Quarantined unreadable storage file {} as {} and skipped it: {reason}",
            path.display(),
            quarantined.display()
        );
    }

    /// List all files in directory with extension
    async fn list_files(&self, dir: &Path, extension: &str) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        let mut entries = fs::read_dir(dir).await.map_err(|e| {
            MqttError::Io(format!("Failed to read directory {}: {}", dir.display(), e))
        })?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| MqttError::Io(format!("Failed to read directory entry: {e}")))?
        {
            let path = entry.path();
            let is_file = fs::metadata(&path).await.is_ok_and(|m| m.is_file());
            if is_file && path.extension().is_some_and(|ext| ext == extension) {
                files.push(path);
            }
        }

        Ok(files)
    }

    async fn cleanup_expired_inflight(&self) -> Result<usize> {
        let mut removed = 0;
        if self.inflight_dir.exists() {
            if let Ok(mut inflight_entries) = fs::read_dir(&self.inflight_dir).await {
                while let Ok(Some(entry)) = inflight_entries.next_entry().await {
                    let client_dir = entry.path();
                    let is_dir = fs::metadata(&client_dir).await.is_ok_and(|m| m.is_dir());
                    if is_dir {
                        let files = self.list_files(&client_dir, "json").await?;
                        for file_path in files {
                            if let Some(msg) =
                                self.read_file::<InflightMessage>(file_path.clone()).await?
                            {
                                if msg.is_expired() {
                                    if let Err(e) = fs::remove_file(&file_path).await {
                                        warn!("failed to remove expired inflight: {e}");
                                    } else {
                                        removed += 1;
                                    }
                                }
                            }
                        }

                        if let Ok(mut dir) = fs::read_dir(&client_dir).await {
                            if dir.next_entry().await.ok().flatten().is_none() {
                                let _ = fs::remove_dir(&client_dir).await;
                            }
                        }
                    }
                }
            }
        }
        Ok(removed)
    }
}

impl StorageBackend for FileBackend {
    async fn store_retained_message(&self, topic: &str, message: RetainedMessage) -> Result<()> {
        let filename = format!("{}.json", Self::topic_to_filename(topic));
        let path = self.retained_dir.join(filename);

        debug!("Storing retained message for topic: {}", topic);
        self.write_file_atomic(path, &message).await?;

        Ok(())
    }

    async fn get_retained_message(&self, topic: &str) -> Result<Option<RetainedMessage>> {
        let filename = format!("{}.json", Self::topic_to_filename(topic));
        let path = self.retained_dir.join(filename);

        let message: Option<RetainedMessage> = self.read_file(path).await?;

        // Check if message has expired
        if let Some(ref msg) = message {
            if msg.is_expired() {
                self.remove_retained_message(topic).await?;
                return Ok(None);
            }
        }

        Ok(message)
    }

    async fn remove_retained_message(&self, topic: &str) -> Result<()> {
        let filename = format!("{}.json", Self::topic_to_filename(topic));
        let path = self.retained_dir.join(filename);

        if path.exists() {
            fs::remove_file(&path).await.map_err(|e| {
                MqttError::Io(format!("Failed to remove retained message file: {e}"))
            })?;
            debug!("Removed retained message for topic: {}", topic);
        }

        Ok(())
    }

    async fn get_retained_messages(
        &self,
        topic_filter: &str,
    ) -> Result<Vec<(String, RetainedMessage)>> {
        let files = self.list_files(&self.retained_dir, "json").await?;
        let mut messages = Vec::new();

        for file_path in files {
            if let Some(filename) = file_path.file_stem().and_then(|s| s.to_str()) {
                let topic = Self::filename_to_topic(filename);

                if topic_matches_filter(&topic, topic_filter) {
                    if let Some(message) = self.read_file::<RetainedMessage>(file_path).await? {
                        if !message.is_expired() {
                            messages.push((topic, message));
                        }
                    }
                }
            }
        }

        Ok(messages)
    }

    async fn store_session(&self, session: ClientSession) -> Result<()> {
        let client_id = session.client_id.clone();
        self.sessions_cache
            .write()
            .await
            .insert(client_id.clone(), session);
        self.dirty_sessions.write().await.insert(client_id);
        Ok(())
    }

    async fn get_session(&self, client_id: &str) -> Result<Option<ClientSession>> {
        let cached = self.sessions_cache.read().await.get(client_id).cloned();
        if let Some(session) = cached {
            if session.is_expired() {
                self.remove_session(client_id).await?;
                return Ok(None);
            }
            return Ok(Some(session));
        }

        let filename = format!("{client_id}.json");
        let path = self.sessions_dir.join(filename);
        let session: Option<ClientSession> = self.read_file(path).await?;

        if let Some(ref sess) = session {
            if sess.is_expired() {
                self.remove_session(client_id).await?;
                return Ok(None);
            }
            self.sessions_cache
                .write()
                .await
                .insert(client_id.to_string(), sess.clone());
        }

        Ok(session)
    }

    async fn remove_session(&self, client_id: &str) -> Result<()> {
        self.sessions_cache.write().await.remove(client_id);
        self.dirty_sessions.write().await.remove(client_id);

        let filename = format!("{client_id}.json");
        let path = self.sessions_dir.join(filename);
        if path.exists() {
            fs::remove_file(&path)
                .await
                .map_err(|e| MqttError::Io(format!("Failed to remove session file: {e}")))?;
            debug!("Removed session for client: {}", client_id);
        }

        Ok(())
    }

    async fn queue_message(&self, message: QueuedMessage) -> Result<()> {
        let client_dir = self.queues_dir.join(&message.client_id);
        fs::create_dir_all(&client_dir).await.map_err(|e| {
            MqttError::Configuration(format!(
                "Failed to create queue dir for {}: {}",
                message.client_id, e
            ))
        })?;

        let timestamp = message.queued_at_secs * 1000;
        let seq = self.queue_seq.fetch_add(1, Ordering::Relaxed);
        let filename = format!("{timestamp}_{seq}.json");
        let path = client_dir.join(filename);

        debug!("Queuing message for client: {}", message.client_id);
        self.write_file_atomic(path, &message).await?;

        Ok(())
    }

    async fn get_queued_messages(&self, client_id: &str) -> Result<Vec<QueuedMessage>> {
        let client_dir = self.queues_dir.join(client_id);
        if !client_dir.exists() {
            return Ok(Vec::new());
        }

        let files = self.list_files(&client_dir, "json").await?;
        let mut messages = Vec::new();

        for file_path in files {
            if let Some(mut message) = self.read_file::<QueuedMessage>(file_path.clone()).await? {
                message.recompute_expiry();
                if !message.is_expired() {
                    messages.push(message);
                } else if let Err(e) = fs::remove_file(&file_path).await {
                    warn!("Failed to remove expired queued message: {e}");
                }
            }
        }

        messages.sort_by_key(|msg| msg.queued_at_secs);

        Ok(messages)
    }

    async fn remove_queued_messages(&self, client_id: &str) -> Result<()> {
        let client_dir = self.queues_dir.join(client_id);
        if client_dir.exists() {
            fs::remove_dir_all(&client_dir).await.map_err(|e| {
                MqttError::Io(format!("Failed to remove queue dir for {client_id}: {e}"))
            })?;
            debug!("Removed all queued messages for client: {}", client_id);
        }

        Ok(())
    }

    async fn store_inflight_message(&self, message: InflightMessage) -> Result<()> {
        let client_dir = self.inflight_dir.join(&message.client_id);
        let direction_tag = match message.direction {
            InflightDirection::Inbound => "inbound",
            InflightDirection::Outbound => "outbound",
        };
        let filename = format!("{direction_tag}_{}.json", message.packet_id);
        let path = client_dir.join(filename);

        self.write_file_atomic(path, &message).await
    }

    async fn get_inflight_messages(&self, client_id: &str) -> Result<Vec<InflightMessage>> {
        let client_dir = self.inflight_dir.join(client_id);
        if !client_dir.exists() {
            return Ok(Vec::new());
        }

        let files = self.list_files(&client_dir, "json").await?;
        let mut messages = Vec::new();

        for file_path in files {
            if let Some(msg) = self.read_file::<InflightMessage>(file_path.clone()).await? {
                if !msg.is_expired() {
                    messages.push(msg);
                } else if let Err(e) = fs::remove_file(&file_path).await {
                    warn!("failed to remove expired inflight file: {e}");
                }
            }
        }

        Ok(messages)
    }

    async fn remove_inflight_message(
        &self,
        client_id: &str,
        packet_id: u16,
        direction: InflightDirection,
    ) -> Result<()> {
        let client_dir = self.inflight_dir.join(client_id);
        let direction_tag = match direction {
            InflightDirection::Inbound => "inbound",
            InflightDirection::Outbound => "outbound",
        };
        let filename = format!("{direction_tag}_{packet_id}.json");
        let path = client_dir.join(filename);

        if path.exists() {
            fs::remove_file(&path)
                .await
                .map_err(|e| MqttError::Io(format!("failed to remove inflight file: {e}")))?;
        }

        if client_dir.exists() {
            if let Ok(mut dir) = fs::read_dir(&client_dir).await {
                if dir.next_entry().await.ok().flatten().is_none() {
                    let _ = fs::remove_dir(&client_dir).await;
                }
            }
        }

        Ok(())
    }

    async fn remove_all_inflight_messages(&self, client_id: &str) -> Result<()> {
        let client_dir = self.inflight_dir.join(client_id);
        if client_dir.exists() {
            fs::remove_dir_all(&client_dir)
                .await
                .map_err(|e| MqttError::Io(format!("failed to remove inflight dir: {e}")))?;
        }
        Ok(())
    }

    async fn cleanup_expired(&self) -> Result<()> {
        let mut removed_count = 0;

        // Clean expired retained messages
        let retained_files = self.list_files(&self.retained_dir, "json").await?;
        for file_path in retained_files {
            if let Some(message) = self.read_file::<RetainedMessage>(file_path.clone()).await? {
                if message.is_expired() {
                    if let Err(e) = fs::remove_file(&file_path).await {
                        warn!("Failed to remove expired retained message: {e}");
                    } else {
                        removed_count += 1;
                    }
                }
            }
        }

        // Clean expired sessions
        let session_files = self.list_files(&self.sessions_dir, "json").await?;
        for file_path in session_files {
            if let Some(session) = self.read_file::<ClientSession>(file_path.clone()).await? {
                if session.is_expired() {
                    if let Err(e) = fs::remove_file(&file_path).await {
                        warn!("Failed to remove expired session: {e}");
                    } else {
                        removed_count += 1;
                    }
                }
            }
        }

        // Clean expired queued messages
        let mut queue_entries = fs::read_dir(&self.queues_dir)
            .await
            .map_err(|e| MqttError::Io(format!("Failed to read queues directory: {e}")))?;

        while let Some(entry) = queue_entries
            .next_entry()
            .await
            .map_err(|e| MqttError::Io(format!("Failed to read queue entry: {e}")))?
        {
            let client_dir = entry.path();
            let is_dir = fs::metadata(&client_dir).await.is_ok_and(|m| m.is_dir());
            if is_dir {
                let queue_files = self.list_files(&client_dir, "json").await?;
                for file_path in queue_files {
                    if let Some(message) =
                        self.read_file::<QueuedMessage>(file_path.clone()).await?
                    {
                        if message.is_expired() {
                            if let Err(e) = fs::remove_file(&file_path).await {
                                warn!("Failed to remove expired queued message: {e}");
                            } else {
                                removed_count += 1;
                            }
                        }
                    }
                }

                // Remove empty client directories
                if let Ok(mut dir) = fs::read_dir(&client_dir).await {
                    if dir.next_entry().await.ok().flatten().is_none() {
                        if let Err(e) = fs::remove_dir(&client_dir).await {
                            warn!("Failed to remove empty queue directory: {e}");
                        }
                    }
                }
            }
        }

        removed_count += self.cleanup_expired_inflight().await?;

        if removed_count > 0 {
            info!("Cleaned up {} expired storage entries", removed_count);
        }

        Ok(())
    }

    async fn flush_sessions(&self) -> Result<()> {
        FileBackend::flush_sessions(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::storage::RetainedMessage;
    use crate::packet::publish::PublishPacket;
    use crate::QoS;

    fn retained(topic: &str, payload: Vec<u8>) -> RetainedMessage {
        let mut packet = PublishPacket::new(topic.to_string(), payload, QoS::AtMostOnce);
        packet.retain = true;
        RetainedMessage::new(packet)
    }

    /// A file the broker cannot read must not stop it from loading the rest.
    ///
    /// This is the failure that took a broker down: an empty retained file made
    /// `get_retained_messages` fail, which failed `router.initialize()`, which made `run()`
    /// return before any listener was bound.
    #[tokio::test]
    async fn unreadable_retained_file_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path()).await.unwrap();

        backend
            .store_retained_message("good/topic", retained("good/topic", b"keep me".to_vec()))
            .await
            .unwrap();

        let corrupt = dir.path().join("retained").join(format!(
            "{}.json",
            FileBackend::topic_to_filename("bad/topic")
        ));
        fs::write(&corrupt, b"").await.unwrap();

        let messages = backend
            .get_retained_messages("#")
            .await
            .expect("an unreadable file must not fail the load");

        assert_eq!(messages.len(), 1, "the readable message must still load");
        assert_eq!(messages[0].0, "good/topic");
        assert!(!corrupt.exists(), "the bad file must be moved aside");
        assert!(
            corrupt.with_extension("corrupt").exists(),
            "the bad file must be quarantined for diagnosis, not deleted"
        );
    }

    /// Truncated (rather than empty) JSON must be tolerated the same way.
    #[tokio::test]
    async fn partially_written_retained_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path()).await.unwrap();

        let corrupt = dir.path().join("retained").join(format!(
            "{}.json",
            FileBackend::topic_to_filename("bad/topic")
        ));
        fs::write(&corrupt, b"{\"payload\":[1,2").await.unwrap();

        let messages = backend.get_retained_messages("#").await.unwrap();
        assert!(messages.is_empty());
        assert!(corrupt.with_extension("corrupt").exists());
    }

    /// Concurrent writers to one topic must not truncate each other's temp file.
    ///
    /// A temp name derived only from the destination is shared by every writer of that path,
    /// so one writer's `create` truncates another's in-flight bytes and the victim renames a
    /// zero-length file into place. That is how the zero-byte file was produced.
    #[tokio::test]
    async fn concurrent_writes_to_one_topic_never_leave_it_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(FileBackend::new(dir.path()).await.unwrap());

        for _ in 0..20 {
            let writers: Vec<_> = (0..8)
                .map(|i| {
                    let backend = Arc::clone(&backend);
                    tokio::spawn(async move {
                        backend
                            .store_retained_message("hot/topic", retained("hot/topic", vec![i; 64]))
                            .await
                    })
                })
                .collect();
            for w in writers {
                w.await.unwrap().unwrap();
            }

            let messages = backend.get_retained_messages("hot/topic").await.unwrap();
            assert_eq!(
                messages.len(),
                1,
                "a concurrently written retained message must always be readable"
            );
        }
    }

    /// A failed write must not leave its temp file behind now that temp names are unique.
    #[tokio::test]
    async fn successful_write_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path()).await.unwrap();
        backend
            .store_retained_message("some/topic", retained("some/topic", b"payload".to_vec()))
            .await
            .unwrap();

        let mut entries = fs::read_dir(dir.path().join("retained")).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(!name.contains(".tmp"), "temp file left behind: {name}");
        }
    }
}
