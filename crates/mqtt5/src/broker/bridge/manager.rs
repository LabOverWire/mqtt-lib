//! Bridge manager for handling multiple bridge connections

use crate::broker::bridge::{
    BridgeConfig, BridgeConnection, BridgeError, BridgeStats, LoopPrevention, Result,
};
use crate::broker::router::MessageRouter;
use crate::packet::publish::PublishPacket;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

/// Manages multiple bridge connections
pub struct BridgeManager {
    /// Active bridge connections
    bridges: Arc<RwLock<HashMap<String, Arc<BridgeConnection>>>>,
    /// Bridge tasks
    tasks: Arc<RwLock<HashMap<String, JoinHandle<()>>>>,
    /// Message router
    router: Arc<MessageRouter>,
    /// Loop prevention
    loop_prevention: Arc<LoopPrevention>,
}

impl BridgeManager {
    /// Creates a new bridge manager
    pub fn new(router: Arc<MessageRouter>) -> Self {
        Self {
            bridges: Arc::new(RwLock::new(HashMap::new())),
            tasks: Arc::new(RwLock::new(HashMap::new())),
            router,
            loop_prevention: Arc::new(LoopPrevention::default()),
        }
    }

    /// Adds a new bridge
    pub async fn add_bridge(&self, config: BridgeConfig) -> Result<()> {
        let name = config.name.clone();

        // Check if bridge already exists
        if self.bridges.read().await.contains_key(&name) {
            return Err(BridgeError::ConfigurationError(format!(
                "Bridge '{}' already exists",
                name
            )));
        }

        // Create bridge connection
        let bridge = Arc::new(BridgeConnection::new(config, self.router.clone())?);

        // Start the bridge
        Box::pin(bridge.start()).await?;

        // Spawn bridge task
        let bridge_clone = bridge.clone();
        let task = tokio::spawn(async move {
            if let Err(e) = Box::pin(bridge_clone.run()).await {
                error!("Bridge task error: {}", e);
            }
        });

        // Store bridge and task
        self.bridges.write().await.insert(name.clone(), bridge);
        self.tasks.write().await.insert(name.clone(), task);

        info!("Added bridge '{}'", name);
        Ok(())
    }

    /// Removes a bridge
    pub async fn remove_bridge(&self, name: &str) -> Result<()> {
        // Get and remove the bridge
        let bridge = self.bridges.write().await.remove(name);

        if let Some(bridge) = bridge {
            // Stop the bridge
            bridge.stop().await?;

            // Cancel the task
            if let Some(task) = self.tasks.write().await.remove(name) {
                task.abort();
            }

            info!("Removed bridge '{}'", name);
            Ok(())
        } else {
            Err(BridgeError::ConfigurationError(format!(
                "Bridge '{}' not found",
                name
            )))
        }
    }

    /// Handles outgoing messages (called by MessageRouter)
    pub async fn handle_outgoing(&self, packet: &PublishPacket) -> Result<()> {
        if packet.topic_name.starts_with("$SYS/") {
            return Ok(());
        }

        if !self.loop_prevention.check_message(packet).await {
            debug!("Message loop detected, not forwarding to bridges");
            return Ok(());
        }

        // Forward to all bridges
        let bridges = self.bridges.read().await;
        for (name, bridge) in bridges.iter() {
            if let Err(e) = bridge.forward_message(packet).await {
                error!("Bridge '{}' failed to forward message: {}", name, e);
                // Continue with other bridges even if one fails
            }
        }

        Ok(())
    }

    /// Gets statistics for all bridges
    pub async fn get_all_stats(&self) -> HashMap<String, BridgeStats> {
        let mut stats = HashMap::new();
        let bridges = self.bridges.read().await;

        for (name, bridge) in bridges.iter() {
            stats.insert(name.clone(), bridge.get_stats().await);
        }

        stats
    }

    /// Gets statistics for a specific bridge
    pub async fn get_bridge_stats(&self, name: &str) -> Option<BridgeStats> {
        let bridges = self.bridges.read().await;
        if let Some(bridge) = bridges.get(name) {
            Some(bridge.get_stats().await)
        } else {
            None
        }
    }

    /// Lists all bridge names
    pub async fn list_bridges(&self) -> Vec<String> {
        self.bridges.read().await.keys().cloned().collect()
    }

    /// Stops all bridges
    pub async fn stop_all(&self) -> Result<()> {
        info!("Stopping all bridges");

        // Stop all bridges
        let bridges: Vec<_> = self.bridges.read().await.values().cloned().collect();
        for bridge in bridges {
            if let Err(e) = bridge.stop().await {
                error!("Failed to stop bridge: {}", e);
            }
        }

        // Cancel all tasks
        let mut tasks = self.tasks.write().await;
        for (name, task) in tasks.drain() {
            debug!("Cancelling task for bridge '{}'", name);
            task.abort();
        }

        // Clear bridges
        self.bridges.write().await.clear();

        Ok(())
    }

    /// Reloads bridge configuration
    pub async fn reload_bridge(&self, config: BridgeConfig) -> Result<()> {
        let name = config.name.clone();

        // Remove existing bridge if present
        if self.bridges.read().await.contains_key(&name) {
            self.remove_bridge(&name).await?;
        }

        // Add new bridge with updated config
        Box::pin(self.add_bridge(config)).await
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
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Create test bridge config pointing to our test broker
        let config = BridgeConfig::new("test-bridge", &format!("{}", broker_addr)).add_topic(
            "test/#",
            BridgeDirection::Both,
            QoS::AtMostOnce,
        );

        // Add bridge
        assert!(Box::pin(manager.add_bridge(config.clone())).await.is_ok());

        // Check bridge exists
        let bridges = manager.list_bridges().await;
        assert_eq!(bridges.len(), 1);
        assert!(bridges.contains(&"test-bridge".to_string()));

        // Try to add duplicate
        assert!(Box::pin(manager.add_bridge(config)).await.is_err());

        // Clean up
        broker_handle.abort();

        // Remove bridge
        assert!(manager.remove_bridge("test-bridge").await.is_ok());

        // Check bridge removed
        let bridges = manager.list_bridges().await;
        assert_eq!(bridges.len(), 0);
    }
}
