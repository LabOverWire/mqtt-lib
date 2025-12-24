use crate::prelude::{String, Vec};
pub use crate::protocol::v5::reason_codes::ReasonCode;
use crate::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProtocolVersion {
    V311,
    #[default]
    V5,
}

impl ProtocolVersion {
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            ProtocolVersion::V311 => 4,
            ProtocolVersion::V5 => 5,
        }
    }
}

impl From<ProtocolVersion> for u8 {
    fn from(version: ProtocolVersion) -> Self {
        version.as_u8()
    }
}

impl TryFrom<u8> for ProtocolVersion {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            4 => Ok(ProtocolVersion::V311),
            5 => Ok(ProtocolVersion::V5),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum QoS {
    AtMostOnce = 0,
    AtLeastOnce = 1,
    ExactlyOnce = 2,
}

impl From<u8> for QoS {
    fn from(value: u8) -> Self {
        match value {
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => QoS::AtMostOnce,
        }
    }
}

impl From<QoS> for u8 {
    fn from(qos: QoS) -> Self {
        qos as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishResult {
    QoS0,
    QoS1Or2 { packet_id: u16 },
}

impl PublishResult {
    #[must_use]
    pub fn packet_id(&self) -> Option<u16> {
        match self {
            Self::QoS0 => None,
            Self::QoS1Or2 { packet_id } => Some(*packet_id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectResult {
    pub session_present: bool,
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
            if properties
                .add(
                    crate::protocol::v5::properties::PropertyId::WillDelayInterval,
                    crate::protocol::v5::properties::PropertyValue::FourByteInteger(delay),
                )
                .is_err()
            {
                crate::prelude::warn_log!("Failed to add will delay interval property");
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
                crate::prelude::warn_log!("Failed to add payload format indicator property");
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
                crate::prelude::warn_log!("Failed to add message expiry interval property");
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
                crate::prelude::warn_log!("Failed to add content type property");
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
                crate::prelude::warn_log!("Failed to add response topic property");
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
                crate::prelude::warn_log!("Failed to add correlation data property");
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
                crate::prelude::warn_log!("Failed to add user property");
            }
        }

        properties
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
                crate::prelude::warn_log!("Failed to add payload format indicator property");
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
                crate::prelude::warn_log!("Failed to add message expiry interval property");
            }
        }
        if let Some(val) = props.topic_alias {
            if properties
                .add(PropertyId::TopicAlias, PropertyValue::TwoByteInteger(val))
                .is_err()
            {
                crate::prelude::warn_log!("Failed to add topic alias property");
            }
        }
        if let Some(val) = props.response_topic {
            if properties
                .add(PropertyId::ResponseTopic, PropertyValue::Utf8String(val))
                .is_err()
            {
                crate::prelude::warn_log!("Failed to add response topic property");
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
                crate::prelude::warn_log!("Failed to add correlation data property");
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
                crate::prelude::warn_log!("Failed to add subscription identifier property");
            }
        }
        if let Some(val) = props.content_type {
            if properties
                .add(PropertyId::ContentType, PropertyValue::Utf8String(val))
                .is_err()
            {
                crate::prelude::warn_log!("Failed to add content type property");
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
                crate::prelude::warn_log!("Failed to add user property");
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
            payload: packet.payload.to_vec(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qos_values() {
        assert_eq!(QoS::AtMostOnce as u8, 0);
        assert_eq!(QoS::AtLeastOnce as u8, 1);
        assert_eq!(QoS::ExactlyOnce as u8, 2);
    }

    #[test]
    fn test_qos_from_u8() {
        assert_eq!(QoS::from(0), QoS::AtMostOnce);
        assert_eq!(QoS::from(1), QoS::AtLeastOnce);
        assert_eq!(QoS::from(2), QoS::ExactlyOnce);

        assert_eq!(QoS::from(3), QoS::AtMostOnce);
        assert_eq!(QoS::from(255), QoS::AtMostOnce);
    }

    #[test]
    fn test_qos_into_u8() {
        assert_eq!(u8::from(QoS::AtMostOnce), 0);
        assert_eq!(u8::from(QoS::AtLeastOnce), 1);
        assert_eq!(u8::from(QoS::ExactlyOnce), 2);
    }
}
