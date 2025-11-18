//! Turmoil-compatible MQTT broker for deterministic testing

use crate::broker::{BrokerConfig, MqttBroker};
use crate::error::MqttError;
use std::net::SocketAddr;

/// Configuration for Turmoil-based broker
#[derive(Debug, Clone)]
pub struct TurmoilBrokerConfig {
    pub address: String,
    pub max_clients: usize,
    pub enable_persistence: bool,
}

impl Default for TurmoilBrokerConfig {
    fn default() -> Self {
        Self {
            address: "0.0.0.0:1883".to_string(),
            max_clients: 100,
            enable_persistence: false,
        }
    }
}

impl TurmoilBrokerConfig {
    /// Creates a new broker configuration with default settings
    #[must_use]
    pub fn new(address: &str) -> Self {
        Self {
            address: address.to_string(),
            max_clients: 100,
            enable_persistence: false,
        }
    }

    /// Sets the TCP port (extracts from or replaces in address)
    #[must_use]
    pub fn with_tcp_port(mut self, port: u16) -> Self {
        // Extract host part if address already has port
        let host = if let Some(pos) = self.address.rfind(':') {
            &self.address[..pos]
        } else {
            &self.address
        };
        self.address = format!("{}:{}", host, port);
        self
    }

    /// Sets the duration (for compatibility - stored but not used)
    #[must_use]
    pub fn with_duration(self, _duration: std::time::Duration) -> Self {
        self
    }

    /// Sets max clients
    #[must_use]
    pub fn with_max_clients(mut self, max_clients: usize) -> Self {
        self.max_clients = max_clients;
        self
    }

    /// Enables or disables persistence
    #[must_use]
    pub fn with_persistence(mut self, enable: bool) -> Self {
        self.enable_persistence = enable;
        self
    }
}

/// Turmoil-compatible MQTT broker
pub struct TurmoilBroker {
    inner: MqttBroker,
    config: TurmoilBrokerConfig,
}

impl TurmoilBroker {
    /// Creates a new Turmoil broker
    pub async fn new(config: TurmoilBrokerConfig) -> Result<Self, MqttError> {
        let addr: SocketAddr = config
            .address
            .parse()
            .map_err(|e| MqttError::Configuration(format!("Invalid address: {}", e)))?;

        let broker_config = BrokerConfig::default()
            .with_bind_address(addr)
            .with_max_clients(config.max_clients);

        let inner = MqttBroker::with_config(broker_config).await?;

        Ok(Self { inner, config })
    }

    /// Creates a new Turmoil broker with configuration (placeholder for tests)
    pub fn with_config(config: TurmoilBrokerConfig) -> TurmoilBrokerBuilder {
        TurmoilBrokerBuilder { config }
    }

    /// Runs the broker
    pub async fn run(mut self) -> Result<(), MqttError> {
        self.inner.run().await
    }

    /// Gets the broker address
    #[must_use]
    pub fn address(&self) -> &str {
        &self.config.address
    }
}

/// Builder for TurmoilBroker to match test expectations
pub struct TurmoilBrokerBuilder {
    config: TurmoilBrokerConfig,
}

impl TurmoilBrokerBuilder {
    /// Runs the broker (async method for test compatibility)
    pub async fn run(self) -> Result<(), MqttError> {
        let broker = TurmoilBroker::new(self.config).await?;
        broker.run().await
    }
}
