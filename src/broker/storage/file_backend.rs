//! File-based storage backend for MQTT broker persistence
//!
//! Provides durable storage using organized file structure with atomic operations.

use super::{ClientSession, QueuedMessage, RetainedMessage, StorageBackend};
use crate::error::{MqttError, Result};
use crate::validation::topic_matches_filter;
use serde_json;
use std::path::{Path, PathBuf};
use crate::time::SystemTime;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

/// File-based storage backend
#[derive(Debug)]
#[allow(clippy::struct_field_names)]
pub struct FileBackend {
    /// Base directory for all storage
    _base_dir: PathBuf,
    /// Directory for retained messages
    retained_dir: PathBuf,
    /// Directory for client sessions
    sessions_dir: PathBuf,
    /// Directory for message queues
    queues_dir: PathBuf,
}

impl FileBackend {
    /// Create new file storage backend
    ///
    /// # Errors
    ///
    /// Returns error if directories cannot be created
    pub async fn new(base_dir: impl AsRef<Path>) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        let retained_dir = base_dir.join("retained");
        let sessions_dir = base_dir.join("sessions");
        let queues_dir = base_dir.join("queues");

        // Create directories
        fs::create_dir_all(&retained_dir)
            .await
            .map_err(|e| MqttError::Configuration(format!("Failed to create retained dir: {e}")))?;

        fs::create_dir_all(&sessions_dir)
            .await
            .map_err(|e| MqttError::Configuration(format!("Failed to create sessions dir: {e}")))?;

        fs::create_dir_all(&queues_dir)
            .await
            .map_err(|e| MqttError::Configuration(format!("Failed to create queues dir: {e}")))?;

        info!(
            "Initialized file storage backend at: {}",
            base_dir.display()
        );

        Ok(Self {
            _base_dir: base_dir,
            retained_dir,
            sessions_dir,
            queues_dir,
        })
    }

    /// Convert topic to safe filename
    #[allow(clippy::unused_self)]
    fn topic_to_filename(&self, topic: &str) -> String {
        // Replace MQTT topic separators and wildcards with safe characters
        topic
            .replace('/', "_slash_")
            .replace('+', "_plus_")
            .replace('#', "_hash_")
            .replace('$', "_dollar_")
    }

    /// Convert filename back to topic
    #[allow(clippy::unused_self)]
    fn filename_to_topic(&self, filename: &str) -> String {
        filename
            .replace("_slash_", "/")
            .replace("_plus_", "+")
            .replace("_hash_", "#")
            .replace("_dollar_", "$")
    }

    /// Write data to file atomically
    async fn write_file_atomic<T: serde::Serialize>(&self, path: PathBuf, data: &T) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| MqttError::Io(format!("Failed to create parent directory: {}", e)))?;
        }

        let temp_path = path.with_extension("tmp");

        // Write to temporary file first
        let serialized = serde_json::to_vec_pretty(data)
            .map_err(|e| MqttError::Configuration(format!("Failed to serialize data: {}", e)))?;

        let mut file = File::create(&temp_path)
            .await
            .map_err(|e| MqttError::Io(format!("Failed to create temp file: {}", e)))?;

        file.write_all(&serialized)
            .await
            .map_err(|e| MqttError::Io(format!("Failed to write temp file: {}", e)))?;

        file.flush()
            .await
            .map_err(|e| MqttError::Io(format!("Failed to flush temp file: {}", e)))?;

        drop(file);

        // Atomically move temp file to final location
        fs::rename(&temp_path, &path)
            .await
            .map_err(|e| MqttError::Io(format!("Failed to rename temp file: {}", e)))?;

        Ok(())
    }

    /// Read data from file
    async fn read_file<T: serde::de::DeserializeOwned>(&self, path: PathBuf) -> Result<Option<T>> {
        match fs::read(&path).await {
            Ok(data) => {
                let result = serde_json::from_slice(&data).map_err(|e| {
                    MqttError::Configuration(format!(
                        "Failed to deserialize {}: {}",
                        path.display(),
                        e
                    ))
                })?;
                Ok(Some(result))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(MqttError::Io(format!(
                "Failed to read {}: {}",
                path.display(),
                e
            ))),
        }
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
            .map_err(|e| MqttError::Io(format!("Failed to read directory entry: {}", e)))?
        {
            let path = entry.path();
            if path.is_file() && path.extension().map_or(false, |ext| ext == extension) {
                files.push(path);
            }
        }

        Ok(files)
    }
}

impl StorageBackend for FileBackend {
    async fn store_retained_message(&self, topic: &str, message: RetainedMessage) -> Result<()> {
        let filename = format!("{}.json", self.topic_to_filename(topic));
        let path = self.retained_dir.join(filename);

        debug!("Storing retained message for topic: {}", topic);
        self.write_file_atomic(path, &message).await?;

        Ok(())
    }

    async fn get_retained_message(&self, topic: &str) -> Result<Option<RetainedMessage>> {
        let filename = format!("{}.json", self.topic_to_filename(topic));
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
        let filename = format!("{}.json", self.topic_to_filename(topic));
        let path = self.retained_dir.join(filename);

        if path.exists() {
            fs::remove_file(&path).await.map_err(|e| {
                MqttError::Io(format!("Failed to remove retained message file: {}", e))
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
                let topic = self.filename_to_topic(filename);

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
        let filename = format!("{}.json", session.client_id);
        let path = self.sessions_dir.join(filename);

        debug!("Storing session for client: {}", session.client_id);
        self.write_file_atomic(path, &session).await?;

        Ok(())
    }

    async fn get_session(&self, client_id: &str) -> Result<Option<ClientSession>> {
        let filename = format!("{}.json", client_id);
        let path = self.sessions_dir.join(filename);

        let session: Option<ClientSession> = self.read_file(path).await?;

        // Check if session has expired
        if let Some(ref sess) = session {
            if sess.is_expired() {
                self.remove_session(client_id).await?;
                return Ok(None);
            }
        }

        Ok(session)
    }

    async fn remove_session(&self, client_id: &str) -> Result<()> {
        let filename = format!("{}.json", client_id);
        let path = self.sessions_dir.join(filename);

        if path.exists() {
            fs::remove_file(&path)
                .await
                .map_err(|e| MqttError::Io(format!("Failed to remove session file: {}", e)))?;
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

        // Use timestamp + random suffix for unique filename
        let timestamp = message
            .queued_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let filename = format!("{}_{}.json", timestamp, timestamp % 1_000_000);
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
            if let Some(message) = self.read_file::<QueuedMessage>(file_path.clone()).await? {
                if !message.is_expired() {
                    messages.push(message);
                } else {
                    // Remove expired message
                    if let Err(e) = fs::remove_file(&file_path).await {
                        warn!("Failed to remove expired queued message: {}", e);
                    }
                }
            }
        }

        // Sort by queue time
        messages.sort_by_key(|msg| msg.queued_at);

        Ok(messages)
    }

    async fn remove_queued_messages(&self, client_id: &str) -> Result<()> {
        let client_dir = self.queues_dir.join(client_id);
        if client_dir.exists() {
            fs::remove_dir_all(&client_dir).await.map_err(|e| {
                MqttError::Io(format!(
                    "Failed to remove queue dir for {}: {}",
                    client_id, e
                ))
            })?;
            debug!("Removed all queued messages for client: {}", client_id);
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
                        warn!("Failed to remove expired retained message: {}", e);
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
                        warn!("Failed to remove expired session: {}", e);
                    } else {
                        removed_count += 1;
                    }
                }
            }
        }

        // Clean expired queued messages
        let mut queue_entries = fs::read_dir(&self.queues_dir)
            .await
            .map_err(|e| MqttError::Io(format!("Failed to read queues directory: {}", e)))?;

        while let Some(entry) = queue_entries
            .next_entry()
            .await
            .map_err(|e| MqttError::Io(format!("Failed to read queue entry: {}", e)))?
        {
            let client_dir = entry.path();
            if client_dir.is_dir() {
                let queue_files = self.list_files(&client_dir, "json").await?;
                for file_path in queue_files {
                    if let Some(message) =
                        self.read_file::<QueuedMessage>(file_path.clone()).await?
                    {
                        if message.is_expired() {
                            if let Err(e) = fs::remove_file(&file_path).await {
                                warn!("Failed to remove expired queued message: {}", e);
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
                            warn!("Failed to remove empty queue directory: {}", e);
                        }
                    }
                }
            }
        }

        if removed_count > 0 {
            info!("Cleaned up {} expired storage entries", removed_count);
        }

        Ok(())
    }
}
