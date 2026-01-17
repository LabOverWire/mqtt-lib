use super::QoS;
use crate::prelude::{String, Vec};

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
