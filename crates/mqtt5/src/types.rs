use crate::codec::CodecRegistry;
use crate::error::{MqttError, Result};
use crate::session::SessionConfig;
use mqtt5_protocol::time::Duration;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

pub use mqtt5_protocol::ReasonCode;
pub use mqtt5_protocol::ReconnectConfig;

#[derive(Clone, Default)]
pub struct ConnectOptions {
    pub protocol_options: mqtt5_protocol::ConnectOptions,
    pub session_config: SessionConfig,
    pub reconnect_config: ReconnectConfig,
    pub keepalive_config: Option<mqtt5_protocol::KeepaliveConfig>,
    pub codec_registry: Option<Arc<CodecRegistry>>,
    /// Enable deferred acknowledgement: inbound `QoS` > 0 messages are delivered with
    /// an `AckToken` the application resolves after durable processing. Requires a
    /// persistent session and a bounded receive maximum; see `validate_deferred_ack`.
    pub deferred_ack: bool,
}

impl ConnectOptions {
    #[must_use]
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            protocol_options: mqtt5_protocol::ConnectOptions::new(client_id),
            session_config: SessionConfig::default(),
            reconnect_config: ReconnectConfig::default(),
            keepalive_config: None,
            codec_registry: None,
            deferred_ack: false,
        }
    }

    /// Enables deferred acknowledgement. Connection-wide: every `subscribe_with_ack`
    /// subscription delivers an `AckToken`. Must be paired with a persistent session
    /// and an explicit bounded receive maximum (see `validate_deferred_ack`).
    #[must_use]
    pub fn with_deferred_ack(mut self, on: bool) -> Self {
        self.deferred_ack = on;
        self
    }

    /// Rejects a deferred-ack configuration that would silently lose or wedge messages.
    ///
    /// Deferred ack holds inbound messages unacknowledged until the application acts,
    /// which is only safe when the session survives a reconnect and the outstanding
    /// tokens are bounded. On a clean session an unacked message is lost; with an
    /// unbounded receive maximum held tokens grow memory without limit.
    ///
    /// Both preconditions are carried by CONNECT properties that exist only in
    /// MQTT 5.0, so the whole mechanism is v5-only: on v3.1.1 the session expiry
    /// and receive maximum never reach the wire, leaving the window unbounded
    /// and the broker unaware of the client's intent.
    ///
    /// # Errors
    /// Returns `Configuration` when `deferred_ack` is set together with
    /// `ProtocolVersion::V311`, `clean_start`, a zero or absent session expiry,
    /// or a zero or absent receive maximum.
    pub fn validate_deferred_ack(&self) -> Result<()> {
        if !self.deferred_ack {
            return Ok(());
        }
        if self.protocol_options.protocol_version != ProtocolVersion::V5 {
            return Err(MqttError::Configuration(
                "deferred_ack requires MQTT 5.0: its session expiry and receive maximum \
                 preconditions cannot be signalled on v3.1.1"
                    .to_string(),
            ));
        }
        if self.protocol_options.clean_start {
            return Err(MqttError::Configuration(
                "deferred_ack requires a persistent session: set clean_start(false)".to_string(),
            ));
        }
        match self.protocol_options.properties.session_expiry_interval {
            Some(interval) if interval > 0 => {}
            _ => {
                return Err(MqttError::Configuration(
                    "deferred_ack requires a non-zero session_expiry_interval".to_string(),
                ));
            }
        }
        match self.protocol_options.properties.receive_maximum {
            Some(max) if max > 0 => {}
            _ => {
                return Err(MqttError::Configuration(
                    "deferred_ack requires an explicit non-zero receive_maximum".to_string(),
                ));
            }
        }
        Ok(())
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
        self.keepalive_config = Some(mqtt5_protocol::KeepaliveConfig {
            ping_interval_percent: config.ping_interval_percent,
            timeout_percent,
            lock_retry_attempts: config.lock_retry_attempts,
            lock_retry_delay_ms: config.lock_retry_delay_ms,
        });
        self
    }

    #[must_use]
    pub fn with_codec_registry(mut self, registry: Arc<CodecRegistry>) -> Self {
        self.codec_registry = Some(registry);
        self
    }
}

impl std::fmt::Debug for ConnectOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectOptions")
            .field("protocol_options", &self.protocol_options)
            .field("session_config", &self.session_config)
            .field("reconnect_config", &self.reconnect_config)
            .field("keepalive_config", &self.keepalive_config)
            .field(
                "codec_registry",
                &self.codec_registry.as_ref().map(|_| "CodecRegistry"),
            )
            .field("deferred_ack", &self.deferred_ack)
            .finish()
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
    fn deferred_ack_validation_accepts_a_persistent_bounded_session() {
        let options = ConnectOptions::new("c")
            .with_clean_start(false)
            .with_session_expiry_interval(3600)
            .with_receive_maximum(16)
            .with_deferred_ack(true);
        assert!(options.validate_deferred_ack().is_ok());
    }

    #[test]
    fn deferred_ack_validation_rejects_clean_session_zero_expiry_and_zero_receive_max() {
        let base = ConnectOptions::new("c")
            .with_clean_start(false)
            .with_session_expiry_interval(3600)
            .with_receive_maximum(16)
            .with_deferred_ack(true);

        assert!(base
            .clone()
            .with_clean_start(true)
            .validate_deferred_ack()
            .is_err());
        assert!(base
            .clone()
            .with_session_expiry_interval(0)
            .validate_deferred_ack()
            .is_err());
        assert!(base
            .clone()
            .with_receive_maximum(0)
            .validate_deferred_ack()
            .is_err());

        let no_expiry = ConnectOptions::new("c")
            .with_clean_start(false)
            .with_receive_maximum(16)
            .with_deferred_ack(true);
        assert!(no_expiry.validate_deferred_ack().is_err());
    }

    #[test]
    fn deferred_ack_validation_rejects_v311() {
        let options = ConnectOptions::new("c")
            .with_clean_start(false)
            .with_session_expiry_interval(3600)
            .with_receive_maximum(16)
            .with_protocol_version(ProtocolVersion::V311)
            .with_deferred_ack(true);
        assert!(options.validate_deferred_ack().is_err());
    }

    #[test]
    fn deferred_ack_off_is_accepted_on_v311() {
        let options = ConnectOptions::new("c").with_protocol_version(ProtocolVersion::V311);
        assert!(options.validate_deferred_ack().is_ok());
    }

    #[test]
    fn deferred_ack_off_never_validates() {
        let options = ConnectOptions::new("c").with_clean_start(true);
        assert!(options.validate_deferred_ack().is_ok());
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

    #[test]
    fn test_keepalive_timeout_percent_preserves_lock_retry() {
        let options = ConnectOptions::new("test-client")
            .with_keepalive_config(KeepaliveConfig::new(60, 150).with_lock_retry(50, 25))
            .with_keepalive_timeout_percent(200);

        let stored = options.keepalive_config.unwrap();
        assert_eq!(stored.lock_retry_attempts, 50);
        assert_eq!(stored.lock_retry_delay_ms, 25);
    }
}
