//! MQTT Broker-to-Broker Bridge Implementation
//!
//! Enables connecting multiple MQTT brokers together by having one broker
//! act as a client to another broker, with configurable topic mapping and
//! message flow direction.

pub mod config;
pub mod connection;
pub mod loop_prevention;
pub mod manager;

pub use config::{BridgeConfig, BridgeDirection, TopicMapping};
pub use connection::BridgeConnection;
pub use loop_prevention::LoopPrevention;
pub use manager::BridgeManager;

use crate::time::Instant;

/// Statistics for a bridge connection
#[derive(Debug, Clone, Default)]
pub struct BridgeStats {
    /// Whether the bridge is currently connected
    pub connected: bool,
    /// Number of connection attempts
    pub connection_attempts: u64,
    /// Messages sent to remote broker
    pub messages_sent: u64,
    /// Messages received from remote broker
    pub messages_received: u64,
    /// Bytes sent to remote broker
    pub bytes_sent: u64,
    /// Bytes received from remote broker
    pub bytes_received: u64,
    /// Last error message if any
    pub last_error: Option<String>,
    /// Time when connection was established
    pub connected_since: Option<Instant>,
}

/// Result type for bridge operations
pub type Result<T> = std::result::Result<T, BridgeError>;

/// Bridge-specific errors
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// Connection to remote broker failed
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    ConfigurationError(String),

    /// Topic mapping error
    #[error("Topic mapping error: {0}")]
    TopicMappingError(String),

    /// Message loop detected
    #[error("Message loop detected for topic: {0}")]
    LoopDetected(String),

    /// Client error
    #[error("Client error: {0}")]
    ClientError(#[from] crate::error::MqttError),
}
