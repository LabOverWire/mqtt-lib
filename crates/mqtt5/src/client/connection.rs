pub use mqtt5_protocol::connection::{ConnectionEvent, DisconnectReason};
pub use mqtt5_protocol::ReconnectConfig;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Duration;

    #[test]
    fn test_reconnect_config_default() {
        let config = ReconnectConfig::default();
        assert!(config.enabled);
        assert_eq!(config.initial_delay, Duration::from_secs(1));
        assert_eq!(config.max_delay, Duration::from_secs(60));
        assert!((config.backoff_factor() - 2.0).abs() < f64::EPSILON);
        assert_eq!(config.max_attempts, None);
    }

    #[test]
    fn test_reconnect_config_custom() {
        let config = ReconnectConfig {
            enabled: true,
            initial_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(30),
            backoff_factor_tenths: 15,
            max_attempts: Some(5),
        };

        assert!(config.enabled);
        assert_eq!(config.initial_delay, Duration::from_secs(2));
        assert_eq!(config.max_delay, Duration::from_secs(30));
        assert!((config.backoff_factor() - 1.5).abs() < 0.01);
        assert_eq!(config.max_attempts, Some(5));
    }
}
