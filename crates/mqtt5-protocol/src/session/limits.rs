use crate::error::{MqttError, Result};
use crate::prelude::{String, Vec};
use crate::time::{Duration, Instant};
use crate::QoS;

#[derive(Debug, Clone)]
pub struct LimitsConfig {
    pub client_maximum_packet_size: u32,
    pub server_maximum_packet_size: Option<u32>,
    pub default_message_expiry: Option<Duration>,
    pub max_message_expiry: Option<Duration>,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            client_maximum_packet_size: crate::constants::limits::MAX_PACKET_SIZE,
            server_maximum_packet_size: None,
            default_message_expiry: None,
            max_message_expiry: Some(Duration::from_secs(86400 * 7)),
        }
    }
}

#[derive(Debug)]
pub struct LimitsManager {
    config: LimitsConfig,
}

impl LimitsManager {
    #[must_use]
    pub fn new(config: LimitsConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(LimitsConfig::default())
    }

    pub fn set_server_maximum_packet_size(&mut self, size: u32) {
        self.config.server_maximum_packet_size = Some(size);
    }

    pub fn set_client_maximum_packet_size(&mut self, size: u32) {
        self.config.client_maximum_packet_size = size;
    }

    #[must_use]
    pub fn effective_maximum_packet_size(&self) -> u32 {
        match self.config.server_maximum_packet_size {
            Some(server_max) if server_max > 0 && self.config.client_maximum_packet_size > 0 => {
                server_max.min(self.config.client_maximum_packet_size)
            }
            Some(server_max) if server_max > 0 => server_max,
            _ => self.config.client_maximum_packet_size,
        }
    }

    /// # Errors
    /// Returns `PacketTooLarge` if the size exceeds the effective maximum.
    pub fn check_packet_size(&self, size: usize) -> Result<()> {
        let max_size = self.effective_maximum_packet_size();
        if max_size > 0 && size > max_size as usize {
            Err(MqttError::PacketTooLarge {
                size,
                max: max_size as usize,
            })
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub fn calculate_message_expiry(&self, expiry_interval: Option<u32>) -> Option<Instant> {
        let interval = match expiry_interval {
            Some(seconds) => Duration::from_secs(u64::from(seconds)),
            None => self.config.default_message_expiry?,
        };

        let final_interval = match self.config.max_message_expiry {
            Some(max) => interval.min(max),
            None => interval,
        };

        Some(Instant::now() + final_interval)
    }

    #[must_use]
    pub fn is_message_expired(&self, expiry_time: Option<Instant>) -> bool {
        match expiry_time {
            Some(expiry) => Instant::now() > expiry,
            None => false,
        }
    }

    #[must_use]
    pub fn get_remaining_expiry(&self, expiry_time: Option<Instant>) -> Option<u32> {
        match expiry_time {
            Some(expiry) => {
                let now = Instant::now();
                if now < expiry {
                    let remaining = expiry.duration_since(now);
                    Some(u32::try_from(remaining.as_secs()).unwrap_or(u32::MAX))
                } else {
                    Some(0)
                }
            }
            None => None,
        }
    }

    #[must_use]
    pub fn client_maximum_packet_size(&self) -> u32 {
        self.config.client_maximum_packet_size
    }

    #[must_use]
    pub fn server_maximum_packet_size(&self) -> Option<u32> {
        self.config.server_maximum_packet_size
    }
}

#[derive(Debug, Clone)]
pub struct ExpiringMessage {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: QoS,
    pub retain: bool,
    pub packet_id: Option<u16>,
    pub expiry_time: Option<Instant>,
    pub expiry_interval: Option<u32>,
}

impl ExpiringMessage {
    #[must_use]
    pub fn new(
        topic: String,
        payload: Vec<u8>,
        qos: QoS,
        retain: bool,
        packet_id: Option<u16>,
        expiry_interval: Option<u32>,
        limits: &LimitsManager,
    ) -> Self {
        let expiry_time = limits.calculate_message_expiry(expiry_interval);

        Self {
            topic,
            payload,
            qos,
            retain,
            packet_id,
            expiry_time,
            expiry_interval,
        }
    }

    #[must_use]
    pub fn is_expired(&self) -> bool {
        match self.expiry_time {
            Some(expiry) => Instant::now() > expiry,
            None => false,
        }
    }

    #[must_use]
    pub fn remaining_expiry_interval(&self) -> Option<u32> {
        match self.expiry_time {
            Some(expiry) => {
                let now = Instant::now();
                if now < expiry {
                    let remaining = expiry.duration_since(now);
                    Some(u32::try_from(remaining.as_secs()).unwrap_or(u32::MAX))
                } else {
                    Some(0)
                }
            }
            None => self.expiry_interval,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_limits_manager_creation() {
        let limits = LimitsManager::with_defaults();
        assert_eq!(
            limits.client_maximum_packet_size(),
            crate::constants::limits::MAX_PACKET_SIZE
        );
        assert_eq!(limits.server_maximum_packet_size(), None);
    }

    #[test]
    fn test_effective_packet_size() {
        let mut limits = LimitsManager::with_defaults();

        assert_eq!(
            limits.effective_maximum_packet_size(),
            crate::constants::limits::MAX_PACKET_SIZE
        );

        limits.set_server_maximum_packet_size(1_048_576);
        assert_eq!(limits.effective_maximum_packet_size(), 1_048_576);

        let config = LimitsConfig {
            client_maximum_packet_size: 1_048_576,
            ..Default::default()
        };
        let mut limits = LimitsManager::new(config);
        limits.set_server_maximum_packet_size(10_485_760);
        assert_eq!(limits.effective_maximum_packet_size(), 1_048_576);
    }

    #[test]
    fn test_packet_size_checking() {
        let mut limits = LimitsManager::with_defaults();
        limits.set_server_maximum_packet_size(1024);

        assert!(limits.check_packet_size(512).is_ok());
        assert!(limits.check_packet_size(1024).is_ok());

        let result = limits.check_packet_size(2048);
        assert!(result.is_err());
        if let Err(MqttError::PacketTooLarge { size, max }) = result {
            assert_eq!(size, 2048);
            assert_eq!(max, 1024);
        }
    }

    #[test]
    fn test_message_expiry() {
        let config = LimitsConfig {
            default_message_expiry: Some(Duration::from_secs(60)),
            ..Default::default()
        };
        let limits = LimitsManager::new(config);

        let expiry_time = limits.calculate_message_expiry(Some(30));
        assert!(expiry_time.is_some());

        let expiry_time = limits.calculate_message_expiry(None);
        assert!(expiry_time.is_some());

        let past_time = Some(Instant::now().checked_sub(Duration::from_secs(10)).unwrap());
        assert!(limits.is_message_expired(past_time));

        let future_time = Some(Instant::now() + Duration::from_secs(10));
        assert!(!limits.is_message_expired(future_time));
    }

    #[test]
    fn test_remaining_expiry() {
        let limits = LimitsManager::with_defaults();

        let future_time = Some(Instant::now() + Duration::from_secs(100));
        let remaining = limits.get_remaining_expiry(future_time);
        assert!(remaining.is_some());
        assert!(remaining.unwrap() > 95 && remaining.unwrap() <= 100);

        let past_time = Some(Instant::now().checked_sub(Duration::from_secs(10)).unwrap());
        let remaining = limits.get_remaining_expiry(past_time);
        assert_eq!(remaining, Some(0));
    }

    #[test]
    fn test_expiring_message() {
        let limits = LimitsManager::with_defaults();

        let msg = ExpiringMessage::new(
            "test/topic".into(),
            vec![1, 2, 3],
            QoS::AtLeastOnce,
            false,
            Some(123),
            Some(60),
            &limits,
        );

        assert!(!msg.is_expired());
        assert!(msg.remaining_expiry_interval().is_some());

        let mut msg = ExpiringMessage::new(
            "test/topic".into(),
            vec![1, 2, 3],
            QoS::AtLeastOnce,
            false,
            Some(123),
            Some(0),
            &limits,
        );

        msg.expiry_time = Some(Instant::now().checked_sub(Duration::from_secs(10)).unwrap());
        assert!(msg.is_expired());
        assert_eq!(msg.remaining_expiry_interval(), Some(0));
    }

    #[test]
    fn test_max_expiry_limit() {
        let config = LimitsConfig {
            max_message_expiry: Some(crate::constants::time::DEFAULT_SESSION_EXPIRY),
            ..Default::default()
        };
        let limits = LimitsManager::new(config);

        let expiry_time = limits.calculate_message_expiry(Some(7200));
        assert!(expiry_time.is_some());

        let remaining = limits.get_remaining_expiry(expiry_time);
        assert!(remaining.is_some());
        assert!(
            remaining.unwrap()
                <= u32::try_from(crate::constants::time::DEFAULT_SESSION_EXPIRY.as_secs())
                    .unwrap_or(u32::MAX)
        );
    }
}
