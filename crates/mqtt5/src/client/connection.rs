use crate::error::MqttError;
use crate::time::Duration;

/// Connection events
#[derive(Debug, Clone)]
pub enum ConnectionEvent {
    /// Successfully connected
    Connected {
        /// Session present flag from CONNACK
        session_present: bool,
    },
    /// Disconnected from broker
    Disconnected {
        /// Reason for disconnection
        reason: DisconnectReason,
    },
    /// Attempting to reconnect
    Reconnecting {
        /// Reconnect attempt number
        attempt: u32,
    },
    /// Reconnection failed
    ReconnectFailed {
        /// Error that caused the failure
        error: MqttError,
    },
}

/// Reasons for disconnection
#[derive(Debug, Clone)]
pub enum DisconnectReason {
    /// Client initiated disconnect
    ClientInitiated,
    /// Server closed connection
    ServerClosed,
    /// Network error
    NetworkError(String),
    /// Protocol error
    ProtocolError(String),
    /// Keep-alive timeout
    KeepAliveTimeout,
}

/// Reconnection configuration
#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    /// Whether automatic reconnection is enabled
    pub enabled: bool,
    /// Initial reconnect delay
    pub initial_delay: Duration,
    /// Maximum reconnect delay
    pub max_delay: Duration,
    /// Exponential backoff factor
    pub backoff_factor: f64,
    /// Maximum number of reconnect attempts (None = unlimited)
    pub max_attempts: Option<u32>,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            backoff_factor: 2.0,
            max_attempts: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reconnect_config_default() {
        let config = ReconnectConfig::default();
        assert!(config.enabled);
        assert_eq!(config.initial_delay, Duration::from_secs(1));
        assert_eq!(config.max_delay, Duration::from_secs(60));
        assert!((config.backoff_factor - 2.0).abs() < f64::EPSILON);
        assert_eq!(config.max_attempts, None);
    }
}
