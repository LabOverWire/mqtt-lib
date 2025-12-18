use crate::session::SessionConfig;
use mqtt5_protocol::time::Duration;
use std::ops::{Deref, DerefMut};

pub use mqtt5_protocol::ReasonCode;

#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    pub enabled: bool,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub max_attempts: u32,
    pub backoff_multiplier: f32,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            max_attempts: 0,
            backoff_multiplier: 2.0,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    pub protocol_options: mqtt5_protocol::ConnectOptions,
    pub session_config: SessionConfig,
    pub reconnect_config: ReconnectConfig,
}

impl ConnectOptions {
    #[must_use]
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            protocol_options: mqtt5_protocol::ConnectOptions::new(client_id),
            session_config: SessionConfig::default(),
            reconnect_config: ReconnectConfig::default(),
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
        self.reconnect_config.max_attempts = attempts;
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
    ConnectProperties, ConnectResult, Message, MessageProperties, ProtocolVersion, PublishOptions,
    PublishProperties, PublishResult, RetainHandling, SubscribeOptions, WillMessage,
    WillProperties,
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
