use crate::session::SessionConfig;
use mqtt5_protocol::time::Duration;
use std::ops::{Deref, DerefMut};

pub use mqtt5_protocol::ReasonCode;
pub use mqtt5_protocol::ReconnectConfig;

#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    pub protocol_options: mqtt5_protocol::ConnectOptions,
    pub session_config: SessionConfig,
    pub reconnect_config: ReconnectConfig,
    pub keepalive_config: Option<mqtt5_protocol::KeepaliveConfig>,
}

impl ConnectOptions {
    #[must_use]
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            protocol_options: mqtt5_protocol::ConnectOptions::new(client_id),
            session_config: SessionConfig::default(),
            reconnect_config: ReconnectConfig::default(),
            keepalive_config: None,
        }
    }

    #[must_use]
    pub fn with_keep_alive(mut self, duration: Duration) -> Self {
        self.protocol_options = self.protocol_options.with_keep_alive(duration);
        self
    }

    #[must_use]
    pub fn with_clean_start(mut self, clean: bool) -> Self {
        self.protocol_options = self.protocol_options.with_clean_start(clean);
        self
    }

    #[must_use]
    pub fn with_credentials(
        mut self,
        username: impl Into<String>,
        password: impl AsRef<[u8]>,
    ) -> Self {
        self.protocol_options = self.protocol_options.with_credentials(username, password);
        self
    }

    #[must_use]
    pub fn with_will(mut self, will: mqtt5_protocol::WillMessage) -> Self {
        self.protocol_options = self.protocol_options.with_will(will);
        self
    }

    #[must_use]
    pub fn with_session_expiry_interval(mut self, interval: u32) -> Self {
        self.protocol_options = self.protocol_options.with_session_expiry_interval(interval);
        self
    }

    #[must_use]
    pub fn with_receive_maximum(mut self, receive_maximum: u16) -> Self {
        self.protocol_options = self.protocol_options.with_receive_maximum(receive_maximum);
        self
    }

    #[must_use]
    pub fn with_automatic_reconnect(mut self, enabled: bool) -> Self {
        self.reconnect_config.enabled = enabled;
        self
    }

    #[must_use]
    pub fn with_reconnect_delay(mut self, initial: Duration, max: Duration) -> Self {
        self.reconnect_config.initial_delay = initial;
        self.reconnect_config.max_delay = max;
        self
    }

    #[must_use]
    pub fn with_max_reconnect_attempts(mut self, attempts: u32) -> Self {
        self.reconnect_config.max_attempts = Some(attempts);
        self
    }

    #[must_use]
    pub fn with_protocol_version(mut self, version: mqtt5_protocol::ProtocolVersion) -> Self {
        self.protocol_options = self.protocol_options.with_protocol_version(version);
        self
    }

    #[must_use]
    pub fn with_authentication_method(mut self, method: impl Into<String>) -> Self {
        self.protocol_options = self.protocol_options.with_authentication_method(method);
        self
    }

    #[must_use]
    pub fn with_authentication_data(mut self, data: impl AsRef<[u8]>) -> Self {
        self.protocol_options = self.protocol_options.with_authentication_data(data);
        self
    }

    #[must_use]
    pub fn with_keepalive_config(mut self, config: mqtt5_protocol::KeepaliveConfig) -> Self {
        self.keepalive_config = Some(config);
        self
    }

    #[must_use]
    pub fn with_keepalive_timeout_percent(mut self, timeout_percent: u8) -> Self {
        let config = self.keepalive_config.unwrap_or_default();
        self.keepalive_config = Some(mqtt5_protocol::KeepaliveConfig::new(
            config.ping_interval_percent,
            timeout_percent,
        ));
        self
    }
}

impl Deref for ConnectOptions {
    type Target = mqtt5_protocol::ConnectOptions;

    fn deref(&self) -> &Self::Target {
        &self.protocol_options
    }
}

impl DerefMut for ConnectOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.protocol_options
    }
}

pub use mqtt5_protocol::{
    ConnectProperties, ConnectResult, KeepaliveConfig, Message, MessageProperties, ProtocolVersion,
    PublishOptions, PublishProperties, PublishResult, RetainHandling, SubscribeOptions,
    WillMessage, WillProperties,
};

#[derive(Debug, Clone, Default)]
pub struct ConnectionStats {
    pub messages_sent: u64,
    pub messages_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub connect_time: Option<crate::time::Instant>,
    pub last_message_time: Option<crate::time::Instant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connect_options_default_keepalive_config() {
        let options = ConnectOptions::new("test-client");
        assert!(options.keepalive_config.is_none());
    }

    #[test]
    fn test_connect_options_with_keepalive_config() {
        let config = KeepaliveConfig::new(50, 200);
        let options = ConnectOptions::new("test-client").with_keepalive_config(config);

        let stored = options.keepalive_config.unwrap();
        assert_eq!(stored.ping_interval_percent, 50);
        assert_eq!(stored.timeout_percent, 200);
    }

    #[test]
    fn test_connect_options_with_keepalive_timeout_percent() {
        let options = ConnectOptions::new("test-client").with_keepalive_timeout_percent(250);

        let stored = options.keepalive_config.unwrap();
        assert_eq!(stored.ping_interval_percent, 75);
        assert_eq!(stored.timeout_percent, 250);
    }

    #[test]
    fn test_connect_options_keepalive_timeout_preserves_ping_interval() {
        let options = ConnectOptions::new("test-client")
            .with_keepalive_config(KeepaliveConfig::new(60, 150))
            .with_keepalive_timeout_percent(200);

        let stored = options.keepalive_config.unwrap();
        assert_eq!(stored.ping_interval_percent, 60);
        assert_eq!(stored.timeout_percent, 200);
    }

    #[test]
    fn test_keepalive_config_timeout_calculation() {
        let config = KeepaliveConfig::new(75, 200);
        let keepalive = Duration::from_secs(60);

        let timeout = config.timeout_duration(keepalive);
        assert_eq!(timeout, Duration::from_secs(120));
    }
}
