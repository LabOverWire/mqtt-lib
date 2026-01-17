//! Bridge manager for handling multiple bridge connections

use crate::broker::bridge::{
    BridgeConfig, BridgeConnection, BridgeError, BridgeStats, LoopPrevention, Result,
};
use crate::broker::router::MessageRouter;
use crate::packet::publish::PublishPacket;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

/// Manages multiple bridge connections
pub struct BridgeManager {
    /// Active bridge connections
    bridges: Arc<RwLock<HashMap<String, Arc<BridgeConnection>>>>,
    tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    /// Message router
    router: Arc<MessageRouter>,
    /// Loop prevention
    loop_prevention: Arc<LoopPrevention>,
}

impl BridgeManager {
    #[allow(clippy::must_use_candidate)]
    pub fn new(router: Arc<MessageRouter>) -> Self {
        Self {
            bridges: Arc::new(RwLock::new(HashMap::new())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            router,
            loop_prevention: Arc::new(LoopPrevention::default()),
        }
    }

    /// Adds a new bridge.
    ///
    /// # Errors
    /// Returns an error if the bridge already exists.
    pub fn add_bridge(&self, config: BridgeConfig) -> Result<()> {
        let name = config.name.clone();

        if self.bridges.read().contains_key(&name) {
            return Err(BridgeError::ConfigurationError(format!(
                "Bridge '{name}' already exists"
            )));
        }

        let bridge = Arc::new(BridgeConnection::new(config, self.router.clone())?);

        bridge.start();

        let bridge_clone = bridge.clone();
        let task = tokio::spawn(async move {
            if let Err(e) = Box::pin(bridge_clone.run()).await {
                error!("Bridge task error: {e}");
            }
        });

        let task_name = name.clone();
        self.bridges.write().insert(name, bridge);
        self.tasks.lock().insert(task_name, task);

        Ok(())
    }

    /// Removes a bridge.
    ///
    /// # Errors
    /// Returns an error if the bridge is not found or stop fails.
    pub async fn remove_bridge(&self, name: &str) -> Result<()> {
        // Get and remove the bridge
        let bridge = self.bridges.write().remove(name);

        if let Some(bridge) = bridge {
            // Stop the bridge
            bridge.stop().await?;

            if let Some(task) = self.tasks.lock().remove(name) {
                task.abort();
            }

            info!("Removed bridge '{}'", name);
            Ok(())
        } else {
            Err(BridgeError::ConfigurationError(format!(
                "Bridge '{name}' not found"
            )))
        }
    }

    /// Handles outgoing messages (called by `MessageRouter`).
    ///
    /// # Errors
    /// Returns an error if message forwarding fails.
    pub async fn handle_outgoing(&self, packet: &PublishPacket) -> Result<()> {
        debug!(
            topic = %packet.topic_name,
            payload_len = packet.payload.len(),
            "handle_outgoing called"
        );

        if packet.topic_name.starts_with("$SYS/") {
            debug!(topic = %packet.topic_name, "skipping $SYS topic");
            return Ok(());
        }

        if !self.loop_prevention.check_message(packet).await {
            debug!(
                topic = %packet.topic_name,
                "Message loop detected, not forwarding to bridges"
            );
            return Ok(());
        }

        let bridge_list: Vec<_> = {
            let bridges = self.bridges.read();
            bridges
                .iter()
                .map(|(name, bridge)| (name.clone(), bridge.clone()))
                .collect()
        };

        if bridge_list.is_empty() {
            debug!(topic = %packet.topic_name, "no bridges configured");
            return Ok(());
        }

        debug!(
            topic = %packet.topic_name,
            bridge_count = bridge_list.len(),
            "forwarding to bridges"
        );

        for (name, bridge) in bridge_list {
            debug!(bridge = %name, topic = %packet.topic_name, "calling forward_message");
            if let Err(e) = bridge.forward_message(packet).await {
                error!("Bridge '{}' failed to forward message: {}", name, e);
            }
        }

        Ok(())
    }

    /// Gets statistics for all bridges
    pub async fn get_all_stats(&self) -> HashMap<String, BridgeStats> {
        let bridge_list: Vec<_> = {
            let bridges = self.bridges.read();
            bridges
                .iter()
                .map(|(name, bridge)| (name.clone(), bridge.clone()))
                .collect()
        };

        let mut stats = HashMap::new();
        for (name, bridge) in bridge_list {
            stats.insert(name, bridge.get_stats().await);
        }
        stats
    }

    /// Gets statistics for a specific bridge
    pub async fn get_bridge_stats(&self, name: &str) -> Option<BridgeStats> {
        let bridge = {
            let bridges = self.bridges.read();
            bridges.get(name).cloned()
        };
        if let Some(bridge) = bridge {
            Some(bridge.get_stats().await)
        } else {
            None
        }
    }

    #[must_use]
    pub fn list_bridges(&self) -> Vec<String> {
        self.bridges.read().keys().cloned().collect()
    }

    /// Stops all bridges.
    ///
    /// # Errors
    /// Returns an error if any bridge fails to stop.
    pub async fn stop_all(&self) -> Result<()> {
        info!("Stopping all bridges");

        // Stop all bridges
        let bridges: Vec<_> = self.bridges.read().values().cloned().collect();
        for bridge in bridges {
            if let Err(e) = bridge.stop().await {
                error!("Failed to stop bridge: {e}");
            }
        }

        let mut tasks = self.tasks.lock();
        for (name, task) in tasks.drain() {
            debug!("Cancelling task for bridge '{}'", name);
            task.abort();
        }

        // Clear bridges
        self.bridges.write().clear();

        Ok(())
    }

    /// Reloads bridge configuration.
    ///
    /// # Errors
    /// Returns an error if removing or adding the bridge fails.
    pub async fn reload_bridge(&self, config: BridgeConfig) -> Result<()> {
        let name = config.name.clone();

        // Remove existing bridge if present
        if self.bridges.read().contains_key(&name) {
            self.remove_bridge(&name).await?;
        }

        // Add new bridge with updated config
        self.add_bridge(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::bridge::BridgeDirection;
    use crate::QoS;

    #[tokio::test]
    async fn test_bridge_manager_lifecycle() {
        use crate::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
        use crate::broker::server::MqttBroker;

        let router = Arc::new(MessageRouter::new());
        let manager = BridgeManager::new(router);

        // Start our own MQTT broker for testing with in-memory storage

        let storage_config = StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        };

        let config = BrokerConfig::default()
            .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
            .with_storage(storage_config);

        let mut broker = MqttBroker::with_config(config)
            .await
            .expect("Failed to create broker");
        let broker_addr = broker.local_addr().expect("Failed to get broker address");

        // Run broker in background
        let broker_handle = tokio::spawn(async move { broker.run().await });

        // Give broker time to start
        tokio::time::sleep(crate::time::Duration::from_millis(100)).await;

        // Create test bridge config pointing to our test broker
        let config = BridgeConfig::new("test-bridge", format!("{broker_addr}")).add_topic(
            "test/#",
            BridgeDirection::Both,
            QoS::AtMostOnce,
        );

        // Add bridge
        assert!(manager.add_bridge(config.clone()).is_ok());

        // Check bridge exists
        let bridges = manager.list_bridges();
        assert_eq!(bridges.len(), 1);
        assert!(bridges.contains(&"test-bridge".to_string()));

        // Try to add duplicate
        assert!(manager.add_bridge(config).is_err());

        // Clean up
        broker_handle.abort();

        // Remove bridge
        assert!(manager.remove_bridge("test-bridge").await.is_ok());

        // Check bridge removed
        let bridges = manager.list_bridges();
        assert_eq!(bridges.len(), 0);
    }
}
