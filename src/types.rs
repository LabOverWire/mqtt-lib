use crate::session::SessionConfig;
use crate::time::Duration;
use crate::QoS;

pub use crate::protocol::v5::reason_codes::ReasonCode;

/// Result of a publish operation
///
/// # Examples
///
/// ```
/// use mqtt5::PublishResult;
///
/// let result = PublishResult::QoS1Or2 { packet_id: 42 };
/// assert_eq!(result.packet_id(), Some(42));
///
/// let result = PublishResult::QoS0;
/// assert_eq!(result.packet_id(), None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishResult {
    /// `QoS` 0 publish completed (no packet ID)
    QoS0,
    /// `QoS` 1 or 2 publish initiated with packet ID
    QoS1Or2 { packet_id: u16 },
}

impl PublishResult {
    /// Get the packet ID if this was a `QoS` 1 or 2 publish
    #[must_use]
    pub fn packet_id(&self) -> Option<u16> {
        match self {
            Self::QoS0 => None,
            Self::QoS1Or2 { packet_id } => Some(*packet_id),
        }
    }
}

/// Result of a connect operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectResult {
    /// Whether a previous session was resumed
    pub session_present: bool,
}

#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    /// Whether automatic reconnection is enabled
    pub enabled: bool,
    /// Initial delay before first reconnection attempt
    pub initial_delay: Duration,
    /// Maximum delay between reconnection attempts
    pub max_delay: Duration,
    /// Maximum number of reconnection attempts (0 = unlimited)
    pub max_attempts: u32,
    /// Exponential backoff multiplier
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

/// Connection options for MQTT client
///
/// # Examples
///
/// ```
/// use mqtt5::{ConnectOptions, WillMessage, QoS};
/// use crate::time::Duration;
///
/// // Basic connection options
/// let options = ConnectOptions::new("my-client-id")
///     .with_clean_start(false)
///     .with_keep_alive(Duration::from_secs(30));
///
/// // With authentication
/// let options = ConnectOptions::new("secure-client")
///     .with_credentials("mqtt_user", b"secure_password");
///
/// // With Last Will and Testament
/// let will = WillMessage::new("status/offline", b"Client disconnected")
///     .with_qos(QoS::AtLeastOnce)
///     .with_retain(true);
///
/// let options = ConnectOptions::new("monitored-client")
///     .with_will(will);
///
/// // With automatic reconnection
/// let options = ConnectOptions::new("persistent-client")
///     .with_automatic_reconnect(true)
///     .with_reconnect_delay(Duration::from_secs(5), Duration::from_secs(60));
/// ```
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub client_id: String,
    pub keep_alive: Duration,
    pub clean_start: bool,
    pub username: Option<String>,
    pub password: Option<Vec<u8>>,
    pub will: Option<WillMessage>,
    pub properties: ConnectProperties,
    pub session_config: SessionConfig,
    pub reconnect_config: ReconnectConfig,
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
            session_config: SessionConfig::default(),
            reconnect_config: ReconnectConfig::default(),
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
            session_config: SessionConfig::default(),
            reconnect_config: ReconnectConfig::default(),
        }
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
        password: impl Into<Vec<u8>>,
    ) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.into());
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
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WillMessage {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: QoS,
    pub retain: bool,
    pub properties: WillProperties,
}

impl WillMessage {
    #[must_use]
    pub fn new(topic: impl Into<String>, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            topic: topic.into(),
            payload: payload.into(),
            qos: QoS::AtMostOnce,
            retain: false,
            properties: WillProperties::default(),
        }
    }

    #[must_use]
    pub fn with_qos(mut self, qos: QoS) -> Self {
        self.qos = qos;
        self
    }

    #[must_use]
    pub fn with_retain(mut self, retain: bool) -> Self {
        self.retain = retain;
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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct WillProperties {
    pub will_delay_interval: Option<u32>,
    pub payload_format_indicator: Option<bool>,
    pub message_expiry_interval: Option<u32>,
    pub content_type: Option<String>,
    pub response_topic: Option<String>,
    pub correlation_data: Option<Vec<u8>>,
    pub user_properties: Vec<(String, String)>,
}

impl From<WillProperties> for crate::protocol::v5::properties::Properties {
    fn from(will_props: WillProperties) -> Self {
        let mut properties = crate::protocol::v5::properties::Properties::default();

        if let Some(delay) = will_props.will_delay_interval {
            // Property type is guaranteed correct by the type system
            if properties
                .add(
                    crate::protocol::v5::properties::PropertyId::WillDelayInterval,
                    crate::protocol::v5::properties::PropertyValue::FourByteInteger(delay),
                )
                .is_err()
            {
                tracing::warn!("Failed to add will delay interval property");
            }
        }

        if let Some(format) = will_props.payload_format_indicator {
            if properties
                .add(
                    crate::protocol::v5::properties::PropertyId::PayloadFormatIndicator,
                    crate::protocol::v5::properties::PropertyValue::Byte(u8::from(format)),
                )
                .is_err()
            {
                tracing::warn!("Failed to add payload format indicator property");
            }
        }

        if let Some(expiry) = will_props.message_expiry_interval {
            if properties
                .add(
                    crate::protocol::v5::properties::PropertyId::MessageExpiryInterval,
                    crate::protocol::v5::properties::PropertyValue::FourByteInteger(expiry),
                )
                .is_err()
            {
                tracing::warn!("Failed to add message expiry interval property");
            }
        }

        if let Some(content_type) = will_props.content_type {
            if properties
                .add(
                    crate::protocol::v5::properties::PropertyId::ContentType,
                    crate::protocol::v5::properties::PropertyValue::Utf8String(content_type),
                )
                .is_err()
            {
                tracing::warn!("Failed to add content type property");
            }
        }

        if let Some(response_topic) = will_props.response_topic {
            if properties
                .add(
                    crate::protocol::v5::properties::PropertyId::ResponseTopic,
                    crate::protocol::v5::properties::PropertyValue::Utf8String(response_topic),
                )
                .is_err()
            {
                tracing::warn!("Failed to add response topic property");
            }
        }

        if let Some(correlation_data) = will_props.correlation_data {
            if properties
                .add(
                    crate::protocol::v5::properties::PropertyId::CorrelationData,
                    crate::protocol::v5::properties::PropertyValue::BinaryData(
                        correlation_data.into(),
                    ),
                )
                .is_err()
            {
                tracing::warn!("Failed to add correlation data property");
            }
        }

        for (key, value) in will_props.user_properties {
            if properties
                .add(
                    crate::protocol::v5::properties::PropertyId::UserProperty,
                    crate::protocol::v5::properties::PropertyValue::Utf8StringPair(key, value),
                )
                .is_err()
            {
                tracing::warn!("Failed to add user property");
            }
        }

        properties
    }
}

/// Options for publishing messages
///
/// # Examples
///
/// ```
/// use mqtt5::{PublishOptions, QoS};
/// use crate::time::Duration;
///
/// // Basic publish with QoS 1
/// let mut options = PublishOptions::default();
/// options.qos = QoS::AtLeastOnce;
///
/// // Retained message with QoS 2
/// let mut options = PublishOptions::default();
/// options.qos = QoS::ExactlyOnce;
/// options.retain = true;
///
/// // With message expiry
/// let mut options = PublishOptions::default();
/// options.properties.message_expiry_interval = Some(3600); // 1 hour
///
/// // With response topic for request/response pattern
/// let mut options = PublishOptions::default();
/// options.properties.response_topic = Some("response/client123".to_string());
/// options.properties.correlation_data = Some(b"req-001".to_vec());
/// ```
#[derive(Debug, Clone)]
pub struct PublishOptions {
    pub qos: QoS,
    pub retain: bool,
    pub properties: PublishProperties,
}

impl Default for PublishOptions {
    fn default() -> Self {
        Self {
            qos: QoS::AtMostOnce,
            retain: false,
            properties: PublishProperties::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PublishProperties {
    pub payload_format_indicator: Option<bool>,
    pub message_expiry_interval: Option<u32>,
    pub topic_alias: Option<u16>,
    pub response_topic: Option<String>,
    pub correlation_data: Option<Vec<u8>>,
    pub user_properties: Vec<(String, String)>,
    pub subscription_identifiers: Vec<u32>,
    pub content_type: Option<String>,
}

impl From<PublishProperties> for crate::protocol::v5::properties::Properties {
    fn from(props: PublishProperties) -> Self {
        use crate::protocol::v5::properties::{Properties, PropertyId, PropertyValue};

        let mut properties = Properties::default();

        if let Some(val) = props.payload_format_indicator {
            if properties
                .add(
                    PropertyId::PayloadFormatIndicator,
                    PropertyValue::Byte(u8::from(val)),
                )
                .is_err()
            {
                tracing::warn!("Failed to add payload format indicator property");
            }
        }
        if let Some(val) = props.message_expiry_interval {
            if properties
                .add(
                    PropertyId::MessageExpiryInterval,
                    PropertyValue::FourByteInteger(val),
                )
                .is_err()
            {
                tracing::warn!("Failed to add message expiry interval property");
            }
        }
        if let Some(val) = props.topic_alias {
            if properties
                .add(PropertyId::TopicAlias, PropertyValue::TwoByteInteger(val))
                .is_err()
            {
                tracing::warn!("Failed to add topic alias property");
            }
        }
        if let Some(val) = props.response_topic {
            if properties
                .add(PropertyId::ResponseTopic, PropertyValue::Utf8String(val))
                .is_err()
            {
                tracing::warn!("Failed to add response topic property");
            }
        }
        if let Some(val) = props.correlation_data {
            if properties
                .add(
                    PropertyId::CorrelationData,
                    PropertyValue::BinaryData(val.into()),
                )
                .is_err()
            {
                tracing::warn!("Failed to add correlation data property");
            }
        }
        for id in props.subscription_identifiers {
            if properties
                .add(
                    PropertyId::SubscriptionIdentifier,
                    PropertyValue::VariableByteInteger(id),
                )
                .is_err()
            {
                tracing::warn!("Failed to add subscription identifier property");
            }
        }
        if let Some(val) = props.content_type {
            if properties
                .add(PropertyId::ContentType, PropertyValue::Utf8String(val))
                .is_err()
            {
                tracing::warn!("Failed to add content type property");
            }
        }
        for (key, value) in props.user_properties {
            if properties
                .add(
                    PropertyId::UserProperty,
                    PropertyValue::Utf8StringPair(key, value),
                )
                .is_err()
            {
                tracing::warn!("Failed to add user property");
            }
        }

        properties
    }
}

#[derive(Debug, Clone)]
pub struct SubscribeOptions {
    pub qos: QoS,
    pub no_local: bool,
    pub retain_as_published: bool,
    pub retain_handling: RetainHandling,
    pub subscription_identifier: Option<u32>,
}

impl Default for SubscribeOptions {
    fn default() -> Self {
        Self {
            qos: QoS::AtMostOnce,
            no_local: false,
            retain_as_published: false,
            retain_handling: RetainHandling::SendAtSubscribe,
            subscription_identifier: None,
        }
    }
}

impl SubscribeOptions {
    #[must_use]
    pub fn with_subscription_identifier(mut self, id: u32) -> Self {
        self.subscription_identifier = Some(id);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainHandling {
    SendAtSubscribe = 0,
    SendIfNew = 1,
    DontSend = 2,
}

/// An MQTT message received from a subscription
///
/// # Examples
///
/// ```
/// use mqtt5::{Message, QoS};
///
/// // Handle different types of messages
/// fn handle_message(msg: Message) {
///     // Access message fields
///     println!("Topic: {}", msg.topic);
///     println!("QoS: {:?}", msg.qos);
///     println!("Retained: {}", msg.retain);
///     
///     // Handle payload as string
///     if let Ok(text) = String::from_utf8(msg.payload.clone()) {
///         println!("Text payload: {}", text);
///     }
///     
///     // Handle payload as bytes
///     println!("Binary payload: {:?}", msg.payload);
///     
///     // Check message properties
///     if let Some(response_topic) = msg.properties.response_topic {
///         println!("Should respond to: {}", response_topic);
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct Message {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: QoS,
    pub retain: bool,
    pub properties: MessageProperties,
}

impl From<crate::packet::publish::PublishPacket> for Message {
    fn from(packet: crate::packet::publish::PublishPacket) -> Self {
        Self {
            topic: packet.topic_name,
            payload: packet.payload,
            qos: packet.qos,
            retain: packet.retain,
            properties: MessageProperties::from(packet.properties),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MessageProperties {
    pub payload_format_indicator: Option<bool>,
    pub message_expiry_interval: Option<u32>,
    pub response_topic: Option<String>,
    pub correlation_data: Option<Vec<u8>>,
    pub user_properties: Vec<(String, String)>,
    pub subscription_identifiers: Vec<u32>,
    pub content_type: Option<String>,
}

impl From<crate::protocol::v5::properties::Properties> for MessageProperties {
    fn from(props: crate::protocol::v5::properties::Properties) -> Self {
        use crate::protocol::v5::properties::{PropertyId, PropertyValue};

        let mut result = Self::default();

        for (id, value) in props.iter() {
            match (id, value) {
                (PropertyId::PayloadFormatIndicator, PropertyValue::Byte(v)) => {
                    result.payload_format_indicator = Some(v != &0);
                }
                (PropertyId::MessageExpiryInterval, PropertyValue::FourByteInteger(v)) => {
                    result.message_expiry_interval = Some(*v);
                }
                (PropertyId::ResponseTopic, PropertyValue::Utf8String(v)) => {
                    result.response_topic = Some(v.clone());
                }
                (PropertyId::CorrelationData, PropertyValue::BinaryData(v)) => {
                    result.correlation_data = Some(v.to_vec());
                }
                (PropertyId::UserProperty, PropertyValue::Utf8StringPair(k, v)) => {
                    result.user_properties.push((k.clone(), v.clone()));
                }
                (PropertyId::SubscriptionIdentifier, PropertyValue::VariableByteInteger(v)) => {
                    result.subscription_identifiers.push(*v);
                }
                (PropertyId::ContentType, PropertyValue::Utf8String(v)) => {
                    result.content_type = Some(v.clone());
                }
                _ => {}
            }
        }

        result
    }
}

impl From<MessageProperties> for PublishProperties {
    fn from(msg_props: MessageProperties) -> Self {
        Self {
            payload_format_indicator: msg_props.payload_format_indicator,
            message_expiry_interval: msg_props.message_expiry_interval,
            topic_alias: None,
            response_topic: msg_props.response_topic,
            correlation_data: msg_props.correlation_data,
            user_properties: msg_props.user_properties,
            subscription_identifiers: msg_props.subscription_identifiers,
            content_type: msg_props.content_type,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionStats {
    pub messages_sent: u64,
    pub messages_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub connect_time: Option<std::time::Instant>,
    pub last_message_time: Option<std::time::Instant>,
}
