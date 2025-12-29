use crate::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepaliveConfig {
    pub ping_interval_percent: u8,
    pub timeout_percent: u8,
}

impl Default for KeepaliveConfig {
    fn default() -> Self {
        Self {
            ping_interval_percent: 75,
            timeout_percent: 150,
        }
    }
}

impl KeepaliveConfig {
    #[must_use]
    pub const fn new(ping_interval_percent: u8, timeout_percent: u8) -> Self {
        Self {
            ping_interval_percent,
            timeout_percent,
        }
    }

    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            ping_interval_percent: 50,
            timeout_percent: 150,
        }
    }

    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub fn ping_interval(&self, keepalive: Duration) -> Duration {
        let millis = keepalive.as_millis() as u64;
        let ping_millis = millis * u64::from(self.ping_interval_percent) / 100;
        Duration::from_millis(ping_millis)
    }

    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub fn timeout_duration(&self, keepalive: Duration) -> Duration {
        let millis = keepalive.as_millis() as u64;
        let timeout_millis = millis * u64::from(self.timeout_percent) / 100;
        Duration::from_millis(timeout_millis)
    }
}

#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn calculate_ping_interval(keepalive: Duration, percent: u8) -> Duration {
    let millis = keepalive.as_millis() as u64;
    let ping_millis = millis * u64::from(percent) / 100;
    Duration::from_millis(ping_millis)
}

#[must_use]
pub fn is_keepalive_timeout(
    time_since_last_ping: Duration,
    last_pong_received: bool,
    keepalive: Duration,
    timeout_percent: u8,
) -> bool {
    let config = KeepaliveConfig::new(0, timeout_percent);
    let timeout = config.timeout_duration(keepalive);
    !last_pong_received && time_since_last_ping > timeout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = KeepaliveConfig::default();
        assert_eq!(config.ping_interval_percent, 75);
        assert_eq!(config.timeout_percent, 150);
    }

    #[test]
    fn test_conservative_config() {
        let config = KeepaliveConfig::conservative();
        assert_eq!(config.ping_interval_percent, 50);
        assert_eq!(config.timeout_percent, 150);
    }

    #[test]
    fn test_ping_interval_calculation() {
        let config = KeepaliveConfig::default();
        let keepalive = Duration::from_secs(60);
        let ping_interval = config.ping_interval(keepalive);
        assert_eq!(ping_interval, Duration::from_secs(45));
    }

    #[test]
    fn test_ping_interval_50_percent() {
        let config = KeepaliveConfig::conservative();
        let keepalive = Duration::from_secs(60);
        let ping_interval = config.ping_interval(keepalive);
        assert_eq!(ping_interval, Duration::from_secs(30));
    }

    #[test]
    fn test_timeout_duration() {
        let config = KeepaliveConfig::default();
        let keepalive = Duration::from_secs(60);
        let timeout = config.timeout_duration(keepalive);
        assert_eq!(timeout, Duration::from_secs(90));
    }

    #[test]
    fn test_calculate_ping_interval_function() {
        let keepalive = Duration::from_secs(60);
        assert_eq!(
            calculate_ping_interval(keepalive, 75),
            Duration::from_secs(45)
        );
        assert_eq!(
            calculate_ping_interval(keepalive, 50),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn test_is_keepalive_timeout_no_pong() {
        let keepalive = Duration::from_secs(60);
        let time_since_ping = Duration::from_secs(100);
        assert!(is_keepalive_timeout(time_since_ping, false, keepalive, 150));
    }

    #[test]
    fn test_is_keepalive_timeout_with_pong() {
        let keepalive = Duration::from_secs(60);
        let time_since_ping = Duration::from_secs(100);
        assert!(!is_keepalive_timeout(time_since_ping, true, keepalive, 150));
    }

    #[test]
    fn test_is_keepalive_timeout_not_expired() {
        let keepalive = Duration::from_secs(60);
        let time_since_ping = Duration::from_secs(80);
        assert!(!is_keepalive_timeout(time_since_ping, false, keepalive, 150));
    }
}
