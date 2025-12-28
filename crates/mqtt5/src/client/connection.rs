pub use mqtt5_protocol::connection::{ConnectionEvent, DisconnectReason};

use crate::time::Duration;

#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    pub enabled: bool,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub backoff_factor: f64,
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

impl ReconnectConfig {
    #[must_use]
    pub fn to_protocol(&self) -> mqtt5_protocol::ReconnectConfig {
        let mut config = mqtt5_protocol::ReconnectConfig {
            enabled: self.enabled,
            initial_delay: self.initial_delay,
            max_delay: self.max_delay,
            max_attempts: self.max_attempts,
            ..Default::default()
        };
        config.set_backoff_factor(self.backoff_factor);
        config
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

    #[test]
    fn test_to_protocol_conversion() {
        let config = ReconnectConfig {
            enabled: true,
            initial_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(30),
            backoff_factor: 1.5,
            max_attempts: Some(5),
        };
        let proto = config.to_protocol();
        assert!(proto.enabled);
        assert_eq!(proto.initial_delay, Duration::from_secs(2));
        assert_eq!(proto.max_delay, Duration::from_secs(30));
        assert!((proto.backoff_factor() - 1.5).abs() < 0.01);
        assert_eq!(proto.max_attempts, Some(5));
    }
}
