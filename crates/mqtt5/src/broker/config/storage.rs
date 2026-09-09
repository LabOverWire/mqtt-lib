use crate::time::Duration;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub backend: StorageBackend,
    pub base_dir: PathBuf,
    #[cfg_attr(not(target_arch = "wasm32"), serde(with = "humantime_serde"))]
    pub cleanup_interval: Duration,
    pub enable_persistence: bool,
    pub max_queued_messages_per_client: usize,
    pub max_queued_bytes_per_client: usize,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: StorageBackend::File,
            base_dir: PathBuf::from("./mqtt_storage"),
            cleanup_interval: Duration::from_secs(3600),
            enable_persistence: true,
            max_queued_messages_per_client: 10_000,
            max_queued_bytes_per_client: 64 * 1024 * 1024,
        }
    }
}

impl StorageConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_backend(mut self, backend: StorageBackend) -> Self {
        self.backend = backend;
        self
    }

    #[must_use]
    pub fn with_base_dir(mut self, dir: PathBuf) -> Self {
        self.base_dir = dir;
        self
    }

    #[must_use]
    pub fn with_cleanup_interval(mut self, interval: Duration) -> Self {
        self.cleanup_interval = interval;
        self
    }

    #[must_use]
    pub fn with_persistence(mut self, enabled: bool) -> Self {
        self.enable_persistence = enabled;
        self
    }

    #[must_use]
    pub fn with_max_queued_messages_per_client(mut self, max_messages: usize) -> Self {
        self.max_queued_messages_per_client = max_messages;
        self
    }

    #[must_use]
    pub fn with_max_queued_bytes_per_client(mut self, max_bytes: usize) -> Self {
        self.max_queued_bytes_per_client = max_bytes;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageBackend {
    File,
    Memory,
}
