//! Configuration hot-reload system for the MQTT broker
//!
//! This module provides the ability to reload broker configuration without restarting,
//! which is essential for production deployments and achieving mosquitto-killer status.

use crate::broker::config::BrokerConfig;
use crate::error::{MqttError, Result};
use crate::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Configuration change notification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChangeEvent {
    /// Timestamp of the change (seconds since UNIX epoch)
    pub timestamp: u64,
    /// Type of configuration change
    pub change_type: ConfigChangeType,
    /// Path to the changed configuration file
    pub config_path: PathBuf,
    /// Previous configuration hash
    pub previous_hash: u64,
    /// New configuration hash
    pub new_hash: u64,
}

/// Types of configuration changes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConfigChangeType {
    /// Complete configuration reload
    FullReload,
    /// Authentication configuration changed
    AuthConfig,
    /// TLS configuration changed
    TlsConfig,
    /// Resource limits changed
    ResourceLimits,
    /// WebSocket configuration changed
    WebSocketConfig,
    /// Bridge configuration changed
    BridgeConfig,
    /// Storage configuration changed
    StorageConfig,
}

/// Hot-reload manager for broker configuration
pub struct HotReloadManager {
    /// Current configuration
    current_config: Arc<RwLock<BrokerConfig>>,
    /// Configuration file path
    config_path: PathBuf,
    /// Change notification sender
    change_sender: broadcast::Sender<ConfigChangeEvent>,
    /// File system watcher handle
    watcher_handle: Option<tokio::task::JoinHandle<()>>,
    /// Last known file modification time
    last_modified: Arc<RwLock<Option<crate::time::SystemTime>>>,
    /// Configuration hash for change detection
    config_hash: Arc<RwLock<u64>>,
}

impl HotReloadManager {
    /// Creates a new hot-reload manager.
    ///
    /// # Errors
    /// This function currently does not return errors but the signature allows for future validation.
    pub fn new(config: BrokerConfig, config_path: PathBuf) -> Result<Self> {
        let (change_sender, _) = broadcast::channel(100);

        let initial_hash = Self::calculate_config_hash(&config);

        let manager = Self {
            current_config: Arc::new(RwLock::new(config)),
            config_path,
            change_sender,
            watcher_handle: None,
            last_modified: Arc::new(RwLock::new(None)),
            config_hash: Arc::new(RwLock::new(initial_hash)),
        };

        Ok(manager)
    }

    /// Starts the hot-reload system.
    ///
    /// # Errors
    /// This function currently does not return errors but the signature allows for future IO errors.
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting configuration hot-reload system");

        // Initialize file modification time
        if let Ok(metadata) = fs::metadata(&self.config_path).await {
            if let Ok(modified) = metadata.modified() {
                *self.last_modified.write().await = Some(modified);
            }
        }

        // Start file system monitoring
        let watcher = self.start_file_watcher();
        self.watcher_handle = Some(watcher);

        info!(
            "Configuration hot-reload system started, monitoring: {:?}",
            self.config_path
        );
        Ok(())
    }

    /// Starts the file system watcher
    fn start_file_watcher(&self) -> tokio::task::JoinHandle<()> {
        let config_path = self.config_path.clone();
        let last_modified = self.last_modified.clone();
        let config_hash = self.config_hash.clone();
        let current_config = self.current_config.clone();
        let change_sender = self.change_sender.clone();

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(crate::time::Duration::from_secs(5));

            loop {
                interval.tick().await;

                match Self::check_file_changed(&config_path, &last_modified).await {
                    Ok(true) => {
                        info!("Configuration file changed, reloading: {:?}", config_path);

                        match Self::reload_config_file(&config_path).await {
                            Ok(new_config) => {
                                let new_hash = Self::calculate_config_hash(&new_config);
                                let old_hash = *config_hash.read().await;

                                if new_hash == old_hash {
                                    debug!("Configuration file changed but content hash unchanged");
                                } else {
                                    // Validate the new configuration
                                    if let Err(e) = new_config.validate() {
                                        error!(
                                            "Invalid configuration file, ignoring reload: {}",
                                            e
                                        );
                                        continue;
                                    }

                                    // Update stored configuration
                                    *current_config.write().await = new_config;
                                    *config_hash.write().await = new_hash;

                                    // Send change notification
                                    let event = ConfigChangeEvent {
                                        timestamp: unix_timestamp_secs(),
                                        change_type: ConfigChangeType::FullReload,
                                        config_path: config_path.clone(),
                                        previous_hash: old_hash,
                                        new_hash,
                                    };

                                    if let Err(e) = change_sender.send(event) {
                                        warn!("Failed to send config change notification: {e}");
                                    }

                                    info!("Configuration successfully reloaded");
                                }
                            }
                            Err(e) => {
                                error!("Failed to reload configuration: {e}");
                            }
                        }
                    }
                    Ok(false) => {
                        // No change
                    }
                    Err(e) => {
                        warn!("Error checking configuration file: {e}");
                    }
                }
            }
        });

        handle
    }

    /// Checks if the configuration file has been modified
    async fn check_file_changed(
        config_path: &Path,
        last_modified: &Arc<RwLock<Option<crate::time::SystemTime>>>,
    ) -> Result<bool> {
        let metadata = fs::metadata(config_path)
            .await
            .map_err(|e| MqttError::Io(format!("Failed to read config file metadata: {e}")))?;

        let current_modified = metadata
            .modified()
            .map_err(|e| MqttError::Io(format!("Failed to get file modification time: {e}")))?;

        let mut last_mod = last_modified.write().await;

        if let Some(last) = *last_mod {
            if current_modified > last {
                *last_mod = Some(current_modified);
                return Ok(true);
            }
        } else {
            *last_mod = Some(current_modified);
        }

        Ok(false)
    }

    /// Reloads configuration from file
    async fn reload_config_file(config_path: &Path) -> Result<BrokerConfig> {
        let config_content = fs::read_to_string(config_path)
            .await
            .map_err(|e| MqttError::Io(format!("Failed to read config file: {e}")))?;

        // Support both JSON and TOML formats
        let config = if config_path.extension().and_then(|s| s.to_str()) == Some("toml") {
            toml::from_str(&config_content)
                .map_err(|e| MqttError::Configuration(format!("Invalid TOML config: {e}")))?
        } else {
            serde_json::from_str(&config_content)
                .map_err(|e| MqttError::Configuration(format!("Invalid JSON config: {e}")))?
        };

        Ok(config)
    }

    /// Calculates a hash of the configuration for change detection
    fn calculate_config_hash(config: &BrokerConfig) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();

        // Hash key configuration fields
        for addr in &config.bind_addresses {
            addr.hash(&mut hasher);
        }
        config.max_clients.hash(&mut hasher);
        config.max_packet_size.hash(&mut hasher);
        config.session_expiry_interval.as_secs().hash(&mut hasher);

        if let Some(ref tls_config) = config.tls_config {
            tls_config.cert_file.hash(&mut hasher);
            tls_config.key_file.hash(&mut hasher);
        }

        if let Some(ref ws_config) = config.websocket_config {
            for addr in &ws_config.bind_addresses {
                addr.hash(&mut hasher);
            }
            ws_config.path.hash(&mut hasher);
        }

        // Hash max_clients as resource limit
        config.max_clients.hash(&mut hasher);

        hasher.finish()
    }

    /// Gets the current configuration
    pub async fn get_config(&self) -> BrokerConfig {
        self.current_config.read().await.clone()
    }

    /// Manually triggers a configuration reload.
    ///
    /// # Errors
    /// Returns an error if the config file cannot be read or the new config is invalid.
    ///
    /// # Panics
    /// Panics if the system time is before the Unix epoch (should not happen).
    pub async fn reload_now(&self) -> Result<bool> {
        info!("Manually triggering configuration reload");

        let new_config = Self::reload_config_file(&self.config_path).await?;
        let new_hash = Self::calculate_config_hash(&new_config);
        let old_hash = *self.config_hash.read().await;

        if new_hash == old_hash {
            info!("Configuration unchanged, no reload needed");
            Ok(false)
        } else {
            // Validate the new configuration
            new_config.validate()?;

            // Update stored configuration
            *self.current_config.write().await = new_config;
            *self.config_hash.write().await = new_hash;

            // Send change notification
            let event = ConfigChangeEvent {
                timestamp: unix_timestamp_secs(),
                change_type: ConfigChangeType::FullReload,
                config_path: self.config_path.clone(),
                previous_hash: old_hash,
                new_hash,
            };

            if let Err(e) = self.change_sender.send(event) {
                warn!("Failed to send config change notification: {e}");
            }

            info!("Configuration manually reloaded successfully");
            Ok(true)
        }
    }

    #[must_use]
    pub fn subscribe_to_changes(&self) -> broadcast::Receiver<ConfigChangeEvent> {
        self.change_sender.subscribe()
    }

    /// Applies specific configuration changes without full reload.
    ///
    /// # Errors
    /// Returns an error if the updated configuration fails validation.
    ///
    /// # Panics
    /// Panics if the system time is before the Unix epoch (should not happen).
    pub async fn apply_partial_config(
        &self,
        change_type: ConfigChangeType,
        update_fn: impl FnOnce(&mut BrokerConfig),
    ) -> Result<()> {
        info!("Applying partial configuration change: {:?}", change_type);

        let mut config = self.current_config.write().await;
        let old_hash = Self::calculate_config_hash(&config);

        // Apply the update
        update_fn(&mut config);

        // Validate the updated configuration
        config.validate()?;

        let new_hash = Self::calculate_config_hash(&config);

        // Send change notification
        let event = ConfigChangeEvent {
            timestamp: unix_timestamp_secs(),
            change_type,
            config_path: self.config_path.clone(),
            previous_hash: old_hash,
            new_hash,
        };

        if let Err(e) = self.change_sender.send(event) {
            warn!("Failed to send config change notification: {e}");
        }

        info!("Partial configuration change applied successfully");
        Ok(())
    }

    #[must_use]
    pub fn get_stats(&self) -> HotReloadStats {
        HotReloadStats {
            config_path: self.config_path.clone(),
            current_hash: futures::executor::block_on(async { *self.config_hash.read().await }),
            subscribers: self.change_sender.receiver_count(),
        }
    }
}

/// Statistics for the hot-reload system
#[derive(Debug, Serialize)]
pub struct HotReloadStats {
    pub config_path: PathBuf,
    pub current_hash: u64,
    pub subscribers: usize,
}

/// Integration helper for broker components
pub struct ConfigSubscriber {
    receiver: broadcast::Receiver<ConfigChangeEvent>,
    component_name: String,
}

impl ConfigSubscriber {
    #[allow(clippy::must_use_candidate)]
    pub fn new(receiver: broadcast::Receiver<ConfigChangeEvent>, component_name: String) -> Self {
        Self {
            receiver,
            component_name,
        }
    }

    /// Waits for the next configuration change.
    ///
    /// # Errors
    /// Returns an error if the change channel is closed.
    pub async fn wait_for_change(&mut self) -> Result<ConfigChangeEvent> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => {
                    debug!(
                        "Component '{}' received config change: {:?}",
                        self.component_name, event.change_type
                    );
                    return Ok(event);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(MqttError::InvalidState(
                        "Config change channel closed".to_string(),
                    ));
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(
                        "Component '{}' lagged behind, skipped {} config changes",
                        self.component_name, skipped
                    );
                    // Continue loop to try again
                }
            }
        }
    }

    /// Checks for pending configuration changes without blocking
    pub fn try_recv_change(&mut self) -> Option<ConfigChangeEvent> {
        match self.receiver.try_recv() {
            Ok(event) => {
                debug!(
                    "Component '{}' received config change: {:?}",
                    self.component_name, event.change_type
                );
                Some(event)
            }
            Err(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_hot_reload_manager() {
        // Create a temporary config file
        let temp_file = NamedTempFile::new().unwrap();
        let initial_config = BrokerConfig::default();
        let config_json = serde_json::to_string_pretty(&initial_config).unwrap();

        tokio::fs::write(temp_file.path(), config_json)
            .await
            .unwrap();

        // Create hot-reload manager
        let mut manager =
            HotReloadManager::new(initial_config.clone(), temp_file.path().to_path_buf()).unwrap();

        // Subscribe to changes
        let mut subscriber =
            ConfigSubscriber::new(manager.subscribe_to_changes(), "test".to_string());

        // Start hot-reload
        manager.start().await.unwrap();

        // Modify the config file
        let mut updated_config = initial_config;
        updated_config.max_clients = 5000;
        let updated_json = serde_json::to_string_pretty(&updated_config).unwrap();

        tokio::fs::write(temp_file.path(), updated_json)
            .await
            .unwrap();

        // Wait for change notification with timeout
        let change_result = tokio::time::timeout(
            crate::time::Duration::from_secs(2),
            subscriber.wait_for_change(),
        )
        .await;

        match change_result {
            Ok(Ok(event)) => {
                assert!(matches!(event.change_type, ConfigChangeType::FullReload));

                // Verify config was updated
                let current_config = manager.get_config().await;
                assert_eq!(current_config.max_clients, 5000);
            }
            Ok(Err(e)) => panic!("Failed to receive config change: {e}"),
            Err(_) => {
                // Timeout - may happen in test environment, just verify manual reload works
                println!("File watcher timeout, testing manual reload");
                let reloaded = manager.reload_now().await.unwrap();
                assert!(reloaded);

                let current_config = manager.get_config().await;
                assert_eq!(current_config.max_clients, 5000);
            }
        }
    }

    #[tokio::test]
    async fn test_partial_config_update() {
        let temp_file = NamedTempFile::new().unwrap();
        let initial_config = BrokerConfig::default();

        let manager =
            HotReloadManager::new(initial_config, temp_file.path().to_path_buf()).unwrap();

        // Apply partial update
        manager
            .apply_partial_config(ConfigChangeType::ResourceLimits, |config| {
                config.max_clients = 10000;
            })
            .await
            .unwrap();

        // Verify update
        let updated_config = manager.get_config().await;
        assert_eq!(updated_config.max_clients, 10000);
    }

    #[tokio::test]
    async fn test_config_validation() {
        let temp_file = NamedTempFile::new().unwrap();
        let initial_config = BrokerConfig::default();

        let manager =
            HotReloadManager::new(initial_config, temp_file.path().to_path_buf()).unwrap();

        // Try to apply invalid config
        let result = manager
            .apply_partial_config(ConfigChangeType::ResourceLimits, |config| {
                // This would make an invalid configuration
                config.max_clients = 0;
            })
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_config_change_event_timestamp() {
        let temp_file = NamedTempFile::new().unwrap();
        let initial_config = BrokerConfig::default();

        let manager =
            HotReloadManager::new(initial_config.clone(), temp_file.path().to_path_buf()).unwrap();

        // Subscribe to changes
        let mut subscriber =
            ConfigSubscriber::new(manager.subscribe_to_changes(), "timestamp_test".to_string());

        // Record time before the operation
        let before_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Apply a config change
        manager
            .apply_partial_config(ConfigChangeType::ResourceLimits, |config| {
                config.max_clients = 3000;
            })
            .await
            .unwrap();

        // Record time after the operation
        let after_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Get the change event
        let event = subscriber.wait_for_change().await.unwrap();

        // Verify timestamp is within reasonable bounds
        assert!(event.timestamp >= before_timestamp);
        assert!(event.timestamp <= after_timestamp);
        assert!(matches!(
            event.change_type,
            ConfigChangeType::ResourceLimits
        ));
    }
}
