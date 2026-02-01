use super::{PublishProperties, QoS};
use crate::prelude::{String, Vec};

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

impl WillProperties {
    pub fn apply_to_publish_properties(
        &self,
        props: &mut crate::protocol::v5::properties::Properties,
    ) {
        if let Some(format) = self.payload_format_indicator {
            props.set_payload_format_indicator(format);
        }
        if let Some(expiry) = self.message_expiry_interval {
            props.set_message_expiry_interval(expiry);
        }
        if let Some(ref content_type) = self.content_type {
            props.set_content_type(content_type.clone());
        }
        if let Some(ref response_topic) = self.response_topic {
            props.set_response_topic(response_topic.clone());
        }
        if let Some(ref correlation_data) = self.correlation_data {
            props.set_correlation_data(correlation_data.clone().into());
        }
        for (key, value) in &self.user_properties {
            props.add_user_property(key.clone(), value.clone());
        }
    }
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
