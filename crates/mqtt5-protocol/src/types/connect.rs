use super::{ProtocolVersion, WillMessage};
use crate::prelude::{String, Vec};
use crate::time::Duration;

#[derive(Clone)]
pub struct ConnectOptions {
    pub client_id: String,
    pub keep_alive: Duration,
    pub clean_start: bool,
    pub username: Option<String>,
    pub password: Option<Vec<u8>>,
    pub will: Option<WillMessage>,
    pub properties: ConnectProperties,
    pub protocol_version: ProtocolVersion,
}

impl core::fmt::Debug for ConnectOptions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ConnectOptions")
            .field("client_id", &self.client_id)
            .field("keep_alive", &self.keep_alive)
            .field("clean_start", &self.clean_start)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("will", &self.will)
            .field("properties", &self.properties)
            .field("protocol_version", &self.protocol_version)
            .finish()
    }
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            keep_alive: Duration::from_secs(60),
            clean_start: true,
            username: None,
            password: None,
            will: None,
            properties: ConnectProperties::default(),
            protocol_version: ProtocolVersion::V5,
        }
    }
}

impl ConnectOptions {
    #[must_use]
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            keep_alive: Duration::from_secs(60),
            clean_start: true,
            username: None,
            password: None,
            will: None,
            properties: ConnectProperties::default(),
            protocol_version: ProtocolVersion::V5,
        }
    }

    #[must_use]
    pub fn with_protocol_version(mut self, version: ProtocolVersion) -> Self {
        self.protocol_version = version;
        self
    }

    #[must_use]
    pub fn with_keep_alive(mut self, duration: Duration) -> Self {
        self.keep_alive = duration;
        self
    }

    #[must_use]
    pub fn with_clean_start(mut self, clean: bool) -> Self {
        self.clean_start = clean;
        self
    }

    #[must_use]
    pub fn with_credentials(
        mut self,
        username: impl Into<String>,
        password: impl AsRef<[u8]>,
    ) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.as_ref().to_vec());
        self
    }

    #[must_use]
    pub fn with_will(mut self, will: WillMessage) -> Self {
        self.will = Some(will);
        self
    }

    #[must_use]
    pub fn with_session_expiry_interval(mut self, interval: u32) -> Self {
        self.properties.session_expiry_interval = Some(interval);
        self
    }

    #[must_use]
    pub fn with_receive_maximum(mut self, receive_maximum: u16) -> Self {
        self.properties.receive_maximum = Some(receive_maximum);
        self
    }

    #[must_use]
    pub fn with_authentication_method(mut self, method: impl Into<String>) -> Self {
        self.properties.authentication_method = Some(method.into());
        self
    }

    #[must_use]
    pub fn with_authentication_data(mut self, data: impl AsRef<[u8]>) -> Self {
        self.properties.authentication_data = Some(data.as_ref().to_vec());
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConnectProperties {
    pub session_expiry_interval: Option<u32>,
    pub receive_maximum: Option<u16>,
    pub maximum_packet_size: Option<u32>,
    pub topic_alias_maximum: Option<u16>,
    pub request_response_information: Option<bool>,
    pub request_problem_information: Option<bool>,
    pub user_properties: Vec<(String, String)>,
    pub authentication_method: Option<String>,
    pub authentication_data: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectResult {
    pub session_present: bool,
}
