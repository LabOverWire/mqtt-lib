//! Cluster simulation utilities for testing distributed MQTT scenarios

use super::{TurmoilBroker, TurmoilBrokerConfig};
use crate::time::Duration;
use std::collections::HashMap;

/// Configuration for a cluster simulation
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    pub brokers: HashMap<String, TurmoilBrokerConfig>,
    pub topology: Vec<(String, String)>, // (broker1, broker2) connections
    pub duration: Duration,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            brokers: HashMap::new(),
            topology: Vec::new(),
            duration: Duration::from_secs(60),
        }
    }
}

impl ClusterConfig {
    /// Creates a new cluster configuration
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a broker to the cluster
    #[must_use]
    pub fn with_broker(mut self, name: String, config: TurmoilBrokerConfig) -> Self {
        self.brokers.insert(name, config);
        self
    }

    /// Adds a connection between two brokers
    #[must_use]
    pub fn with_connection(mut self, broker1: String, broker2: String) -> Self {
        self.topology.push((broker1, broker2));
        self
    }

    /// Sets the simulation duration
    #[must_use]
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }

    /// Creates a simple 3-node cluster
    #[must_use]
    pub fn three_node_cluster() -> Self {
        Self::new()
            .with_broker(
                "broker1".to_string(),
                TurmoilBrokerConfig {
                    address: "broker1:1883".to_string(),
                    ..Default::default()
                },
            )
            .with_broker(
                "broker2".to_string(),
                TurmoilBrokerConfig {
                    address: "broker2:1883".to_string(),
                    ..Default::default()
                },
            )
            .with_broker(
                "broker3".to_string(),
                TurmoilBrokerConfig {
                    address: "broker3:1883".to_string(),
                    ..Default::default()
                },
            )
            .with_connection("broker1".to_string(), "broker2".to_string())
            .with_connection("broker2".to_string(), "broker3".to_string())
            .with_connection("broker3".to_string(), "broker1".to_string())
    }

    /// Creates a hub-and-spoke cluster
    #[must_use]
    pub fn hub_and_spoke(hub: &str, spokes: Vec<String>) -> Self {
        let mut config = Self::new().with_broker(
            hub.to_string(),
            TurmoilBrokerConfig {
                address: format!("{}:1883", hub),
                ..Default::default()
            },
        );

        for spoke in spokes {
            config = config
                .with_broker(
                    spoke.clone(),
                    TurmoilBrokerConfig {
                        address: format!("{}:1883", spoke),
                        ..Default::default()
                    },
                )
                .with_connection(hub.to_string(), spoke);
        }

        config
    }
}

/// Represents a cluster simulation
#[allow(dead_code)]
pub struct ClusterSimulation {
    config: ClusterConfig,
    brokers: HashMap<String, TurmoilBroker>,
}

impl ClusterSimulation {
    /// Creates a new cluster simulation
    pub fn new(config: ClusterConfig) -> Self {
        Self {
            config,
            brokers: HashMap::new(),
        }
    }
}
